// 邮件通道：SMTP 发信 + IMAP 收信。
//
// 为什么要两个协议：**SMTP 只能发，收不到回信**。客户「直接回邮件」的回复
// 只能靠 IMAP（或 POP3 / 服务商 inbound webhook）取回来。所以：
//   - 到「待回复」时用 SMTP 把问题发给客户；
//   - `tba wait` 期间用 IMAP 轮询信箱，把客户的回信**当成任务评论**写回去，
//     后面完全复用原有的「读评论 → resume」逻辑，不需要第二套状态机。
//
// 不引第三方邮件库：协议本身够简单，自己写能少一堆依赖和编译时间。
// 只支持最主流的两种发信口：465（隐式 TLS）和 587/25（STARTTLS）。
use crate::config::Config;
use native_tls::TlsConnector;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

// Read + Write 的组合 trait，方便统一持有「明文 socket」和「TLS socket」。
trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

fn connect(host: &str, port: u16, implicit_tls: bool) -> Result<Box<dyn Stream>, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("解析 {host}:{port} 失败: {e}"))?
        .next()
        .ok_or_else(|| format!("{host}:{port} 解析不到地址"))?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("连接 {host}:{port} 失败: {e}"))?;
    tcp.set_read_timeout(Some(IO_TIMEOUT)).ok();
    tcp.set_write_timeout(Some(IO_TIMEOUT)).ok();
    if implicit_tls {
        let tls = TlsConnector::new()
            .map_err(|e| format!("初始化 TLS 失败: {e}"))?
            .connect(host, tcp)
            .map_err(|e| format!("TLS 握手失败({host}:{port}): {e}"))?;
        Ok(Box::new(tls))
    } else {
        Ok(Box::new(tcp))
    }
}

// ---- SMTP ----

struct Smtp {
    s: BufReader<Box<dyn Stream>>,
    ehlo: String,
}

// 下面几个泛型小函数既给「明文探测阶段」用（BufReader<&TcpStream>），
// 也给「升级后的 TLS 阶段」用（BufReader<TlsStream>），所以不能绑死在具体类型上。
fn read_reply<S: Read + Write>(s: &mut BufReader<S>) -> Result<(u16, String), String> {
    let mut out = String::new();
    let mut code = 0u16;
    loop {
        let mut line = String::new();
        let n = s
            .read_line(&mut line)
            .map_err(|e| format!("读 SMTP 回复失败: {e}"))?;
        if n == 0 {
            return Err("SMTP 连接被对端关闭".to_string());
        }
        if code == 0 {
            code = line
                .get(0..3)
                .and_then(|c| c.parse::<u16>().ok())
                .ok_or_else(|| format!("看不懂的 SMTP 回复: {}", line.trim()))?;
        }
        out.push_str(&line);
        // 第 4 位是空格 = 最后一行；"-" 表示还有后续行。
        if line.as_bytes().get(3) != Some(&b'-') {
            break;
        }
    }
    Ok((code, out))
}

fn write_cmd<S: Read + Write>(s: &mut BufReader<S>, line: &str) -> Result<(), String> {
    s.get_mut()
        .write_all(format!("{line}\r\n").as_bytes())
        .map_err(|e| format!("写 SMTP 命令失败: {e}"))?;
    s.get_mut().flush().map_err(|e| format!("flush 失败: {e}"))
}

fn greet<S: Read + Write>(s: &mut BufReader<S>) -> Result<(), String> {
    let (code, _) = read_reply(s)?;
    if code != 220 {
        return Err(format!("SMTP 服务端未就绪（{code}）"));
    }
    Ok(())
}

fn ehlo<S: Read + Write>(s: &mut BufReader<S>, host: &str) -> Result<String, String> {
    write_cmd(s, &format!("EHLO {host}"))?;
    let (code, text) = read_reply(s)?;
    if code != 250 {
        return Err(format!("EHLO 失败（{code}）: {}", text.trim()));
    }
    Ok(text)
}

impl Smtp {
    // 建连接并完成协商：
    // 465 = 隐式 TLS（连上就是加密的）；其他端口先明文，服务端支持 STARTTLS 就升级。
    fn new(host: &str, port: u16) -> Result<Smtp, String> {
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("解析 {host}:{port} 失败: {e}"))?
            .next()
            .ok_or_else(|| format!("{host}:{port} 解析不到地址"))?;
        let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|e| format!("连接 {host}:{port} 失败: {e}"))?;
        tcp.set_read_timeout(Some(IO_TIMEOUT)).ok();
        tcp.set_write_timeout(Some(IO_TIMEOUT)).ok();

        if port == 465 {
            let tls = TlsConnector::new()
                .map_err(|e| format!("初始化 TLS 失败: {e}"))?
                .connect(host, tcp)
                .map_err(|e| format!("TLS 握手失败({host}:{port}): {e}"))?;
            let mut me = Smtp {
                s: BufReader::new(Box::new(tls)),
                ehlo: String::new(),
            };
            greet(&mut me.s)?;
            me.ehlo = ehlo(&mut me.s, host)?;
            return Ok(me);
        }

        // 明文先谈一轮，看服务端给不给 STARTTLS。
        let upgrade = {
            let mut probe = BufReader::new(&tcp);
            greet(&mut probe)?;
            let text = ehlo(&mut probe, host)?;
            if text.to_uppercase().contains("STARTTLS") {
                write_cmd(&mut probe, "STARTTLS")?;
                let (code, _) = read_reply(&mut probe)?;
                if code != 220 {
                    return Err(format!("STARTTLS 被拒（{code}）"));
                }
                true
            } else {
                false
            }
        };

        if upgrade {
            let tls = TlsConnector::new()
                .map_err(|e| format!("初始化 TLS 失败: {e}"))?
                .connect(host, tcp)
                .map_err(|e| format!("STARTTLS 握手失败: {e}"))?;
            let mut me = Smtp {
                s: BufReader::new(Box::new(tls)),
                ehlo: String::new(),
            };
            // 升级后必须重新 EHLO（协议规定，且新能力集要以加密后的为准）。
            me.ehlo = ehlo(&mut me.s, host)?;
            return Ok(me);
        }

        // 内网 25 端口常见不支持 STARTTLS，明文继续（有密码时会明文传，慎用）。
        let mut me = Smtp {
            s: BufReader::new(Box::new(tcp)),
            ehlo: String::new(),
        };
        me.ehlo = ehlo(&mut me.s, host)?;
        Ok(me)
    }

    fn cmd(&mut self, line: &str) -> Result<(u16, String), String> {
        write_cmd(&mut self.s, line)?;
        read_reply(&mut self.s)
    }

    fn auth(&mut self, user: &str, pass: &str) -> Result<(), String> {
        // 优先 AUTH LOGIN（兼容性最好），失败再试 AUTH PLAIN。
        let (code, _) = self.cmd("AUTH LOGIN")?;
        if code == 334 {
            let (c1, _) = self.cmd(&b64_encode(user.as_bytes()))?;
            if c1 != 334 {
                return Err(format!("SMTP 不接受用户名（{c1}）"));
            }
            let (c2, text) = self.cmd(&b64_encode(pass.as_bytes()))?;
            if c2 != 235 {
                return Err(format!("SMTP 认证失败（{c2}）: {}", text.trim()));
            }
            return Ok(());
        }
        let plain = format!("\0{user}\0{pass}");
        let (c3, text) = self.cmd(&format!("AUTH PLAIN {}", b64_encode(plain.as_bytes())))?;
        if c3 != 235 {
            return Err(format!("SMTP 认证失败（{c3}）: {}", text.trim()));
        }
        Ok(())
    }
}

// 发一封信。subject/body 允许中文（自动 base64 编码）。
pub fn send(cfg: &Config, subject: &str, body: &str) -> Result<(), String> {
    let host = cfg.smtp_host.trim();
    if host.is_empty() {
        return Err("没配 SMTP 服务器（设置里填 SMTP 主机/端口/账号/密码/收件人）".to_string());
    }
    let (from_name, from_addr) = if cfg.smtp_from.trim().is_empty() {
        (None, cfg.smtp_user.trim().to_string())
    } else {
        split_from(&cfg.smtp_from)
    };
    // 显示名不能单独当地址用。QQ 邮箱（及多数 SMTP）会把 `MAIL FROM:<works on teambition>`
    // 当成非法输入直接 502，而且错误只落在 stderr 里，很容易被当成「没发信」。
    if !from_addr.contains('@') {
        return Err(format!(
            "发件地址不合法：`{from_addr}`。smtp_from 要么留空（自动用账号），\
             要么写 `显示名 <邮箱>` 或纯邮箱——显示名不能单独当地址"
        ));
    }
    let tos = recipients(&cfg.mail_to);
    if tos.is_empty() {
        return Err("没配收件人（设置里的「收件人」）".to_string());
    }

    let mut smtp = Smtp::new(host, cfg.smtp_port)?;
    if !cfg.smtp_user.trim().is_empty() {
        // 没声明 AUTH 通常是端口/TLS 配错了（比如 465 当明文用）。
        if !smtp.ehlo.to_uppercase().contains("AUTH") {
            return Err("服务端没声明支持 AUTH，检查端口（465 隐式 TLS / 587 STARTTLS）与账号".into());
        }
        smtp.auth(cfg.smtp_user.trim(), &cfg.smtp_pass)?;
    }
    let (code, text) = smtp.cmd(&format!("MAIL FROM:<{from_addr}>"))?;
    if code != 250 {
        return Err(format!("MAIL FROM 被拒（{code}）: {}", text.trim()));
    }
    for to in &tos {
        let (code, text) = smtp.cmd(&format!("RCPT TO:<{to}>"))?;
        if code != 250 && code != 251 {
            return Err(format!("RCPT TO <{to}> 被拒（{code}）: {}", text.trim()));
        }
    }
    let (code, text) = smtp.cmd("DATA")?;
    if code != 354 {
        return Err(format!("DATA 被拒（{code}）: {}", text.trim()));
    }
    let msg = build_message(from_name.as_deref(), &from_addr, &tos, subject, body);
    smtp.s
        .get_mut()
        .write_all(msg.as_bytes())
        .map_err(|e| format!("写邮件正文失败: {e}"))?;
    let (code, text) = smtp.cmd(".")?;
    if code != 250 {
        return Err(format!("投递失败（{code}）: {}", text.trim()));
    }
    let _ = smtp.cmd("QUIT");
    Ok(())
}

// 解析发件人写法，两种都支持：
//   www.3850@qq.com                        -> (None, "www.3850@qq.com")
//   works on teambition <www.3850@qq.com>  -> (Some("works on teambition"), "www.3850@qq.com")
//
// 为什么必须解析：`MAIL FROM:<...>` 里只能是地址。把显示名直接塞进去会被服务端拒
// （QQ 邮箱返回 502 Invalid input），而且失败只打一行 stderr，很容易被当成「没发信」。
fn split_from(s: &str) -> (Option<String>, String) {
    let t = s.trim();
    if let (Some(l), Some(r)) = (t.find('<'), t.rfind('>')) {
        if r > l {
            let name = t[..l].trim().trim_matches('"').trim();
            let addr = t[l + 1..r].trim();
            if !addr.is_empty() {
                return (
                    if name.is_empty() {
                        None
                    } else {
                        Some(name.to_string())
                    },
                    addr.to_string(),
                );
            }
        }
    }
    (None, t.to_string())
}

fn build_message(
    from_name: Option<&str>,
    from_addr: &str,
    tos: &[String],
    subject: &str,
    body: &str,
) -> String {
    let mut m = String::new();
    // 显示名同样走 RFC2047，中文显示名不会乱码。
    match from_name {
        Some(n) => m.push_str(&format!(
            "From: =?UTF-8?B?{}?= <{from_addr}>\r\n",
            b64_encode(n.as_bytes())
        )),
        None => m.push_str(&format!("From: <{from_addr}>\r\n")),
    }
    m.push_str(&format!("To: {}\r\n", tos.join(", ")));
    // 主题一律 RFC2047 base64，中文/长主题都不会被截断或乱码。
    m.push_str(&format!(
        "Subject: =?UTF-8?B?{}?=\r\n",
        b64_encode(subject.as_bytes())
    ));
    m.push_str(&format!("Date: {}\r\n", chrono::Local::now().to_rfc2822()));
    m.push_str("MIME-Version: 1.0\r\n");
    m.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    m.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
    for line in wrap(&b64_encode(body.as_bytes()), 76) {
        m.push_str(&line);
        m.push_str("\r\n");
    }
    m.push_str(".\r\n");
    m
}

pub fn recipients(list: &str) -> Vec<String> {
    list.split([';', ',', ' ', '\n'])
        .map(|s| s.trim().to_string())
        .filter(|s| s.contains('@'))
        .collect()
}

// 能不能发信：SMTP 主机 + 至少一个合法收件人。
pub fn can_send(cfg: &Config) -> bool {
    !cfg.smtp_host.trim().is_empty() && !recipients(&cfg.mail_to).is_empty()
}

// 能不能收信：IMAP 主机 + 账号（密码跟 SMTP 共用一套）。
pub fn can_receive(cfg: &Config) -> bool {
    !cfg.imap_host.trim().is_empty() && !cfg.smtp_user.trim().is_empty()
}

// ---- IMAP ----

// 诊断用：列出 INBOX 里最近 limit 封邮件的主题和正文摘要（不管读没读、有没有标记）。
//
// 为什么需要它：`mail check` 报「没有回信」有两种完全不同的原因——
// ①信根本没到这个信箱；②到了但主题标记对不上（比如客户改标题、或只是测试信）。
// 光看「没有」分不清，所以把最近几封真实列出来看。
pub struct MailRow {
    pub uid: String,
    pub subject: String,
    pub snippet: String,
}

pub fn recent(cfg: &Config, limit: usize) -> Result<Vec<MailRow>, String> {
    let host = cfg.imap_host.trim();
    if host.is_empty() {
        return Err("没配 IMAP 服务器".to_string());
    }
    let stream = connect(host, cfg.imap_port, true)?;
    let mut s = BufReader::new(stream);
    expect_ok(&mut s, "*")?;
    imap_cmd(
        &mut s,
        "a1",
        &format!(
            "LOGIN {} {}",
            quote(cfg.smtp_user.trim()),
            quote(&cfg.smtp_pass)
        ),
    )?;
    imap_cmd(&mut s, "a2", "SELECT INBOX")?;

    let search = imap_cmd(&mut s, "a3", "UID SEARCH ALL")?;
    let mut uids: Vec<String> = search
        .lines()
        .find(|l| l.starts_with("* SEARCH"))
        .map(|l| {
            l.trim_start_matches("* SEARCH")
                .split_whitespace()
                .map(|x| x.to_string())
                .collect()
        })
        .unwrap_or_default();
    // UID 递增，取尾部就是最新几封。
    if uids.len() > limit {
        uids = uids.split_off(uids.len() - limit);
    }

    let mut out = Vec::new();
    for (i, uid) in uids.iter().enumerate() {
        let raw = imap_fetch(&mut s, &format!("b{i}"), uid)?;
        let (subject, body) = parse_message(&raw);
        let snippet: String = body
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .take(2)
            .collect::<Vec<_>>()
            .join(" / ");
        out.push(MailRow {
            uid: uid.clone(),
            subject,
            snippet: snippet.chars().take(120).collect(),
        });
    }
    let _ = imap_cmd(&mut s, "z9", "LOGOUT");
    out.reverse();
    Ok(out)
}

// 从信箱里捞「主题或正文含 marker」的未读邮件，返回正文纯文本，并标记已读。
pub fn fetch_replies(cfg: &Config, marker: &str) -> Result<Vec<String>, String> {
    let host = cfg.imap_host.trim();
    if host.is_empty() {
        return Err("没配 IMAP 服务器（收邮件必需，SMTP 收不到回信）".to_string());
    }
    let stream = connect(host, cfg.imap_port, true)?;
    let mut s = BufReader::new(stream);
    expect_ok(&mut s, "*")?; // 问候语

    imap_cmd(&mut s, "a1", &format!("LOGIN {} {}", quote(cfg.smtp_user.trim()), quote(&cfg.smtp_pass)))?;
    imap_cmd(&mut s, "a2", "SELECT INBOX")?;

    let search = imap_cmd(&mut s, "a3", "UID SEARCH UNSEEN")?;
    let uids: Vec<String> = search
        .lines()
        .find(|l| l.starts_with("* SEARCH"))
        .map(|l| {
            l.trim_start_matches("* SEARCH")
                .split_whitespace()
                .map(|x| x.to_string())
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for (i, uid) in uids.iter().enumerate() {
        let raw = imap_fetch(&mut s, &format!("b{}", i + 1), uid)?;
        let (subject, body) = parse_message(&raw);
        if subject.contains(marker) || body.contains(marker) {
            out.push(clean_reply(&body, marker));
            let _ = imap_cmd(
                &mut s,
                &format!("c{i}"),
                &format!("UID STORE {uid} +FLAGS (\\Seen)"),
            );
        }
    }
    let _ = imap_cmd(&mut s, "z9", "LOGOUT");
    Ok(out)
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn expect_ok(s: &mut BufReader<Box<dyn Stream>>, tag: &str) -> Result<(), String> {
    let mut buf = String::new();
    loop {
        let mut line = String::new();
        let n = s
            .read_line(&mut line)
            .map_err(|e| format!("读 IMAP 回复失败: {e}"))?;
        if n == 0 {
            return Err("IMAP 连接被对端关闭".to_string());
        }
        buf.push_str(&line);
        if line.starts_with(tag) {
            if line.starts_with(&format!("{tag} OK")) {
                return Ok(());
            }
            return Err(format!("IMAP 失败: {}", line.trim()));
        }
    }
}

fn imap_cmd(s: &mut BufReader<Box<dyn Stream>>, tag: &str, cmd: &str) -> Result<String, String> {
    s.get_mut()
        .write_all(format!("{tag} {cmd}\r\n").as_bytes())
        .map_err(|e| format!("写 IMAP 命令失败: {e}"))?;
    s.get_mut().flush().ok();
    let mut buf = String::new();
    loop {
        let mut line = String::new();
        let n = s
            .read_line(&mut line)
            .map_err(|e| format!("读 IMAP 回复失败: {e}"))?;
        if n == 0 {
            return Err("IMAP 连接被对端关闭".to_string());
        }
        buf.push_str(&line);
        if line.starts_with(tag) {
            if line.starts_with(&format!("{tag} OK")) {
                return Ok(buf);
            }
            return Err(format!("IMAP 失败: {}", line.trim()));
        }
    }
}

// FETCH 一封信的完整原文。IMAP 的字面量（{N}\r\n 后面跟 N 字节）必须按字节读，
// 不能按行读，否则正文里的换行会把结构读乱。
fn imap_fetch(s: &mut BufReader<Box<dyn Stream>>, tag: &str, uid: &str) -> Result<String, String> {
    s.get_mut()
        .write_all(format!("{tag} UID FETCH {uid} BODY.PEEK[]\r\n").as_bytes())
        .map_err(|e| format!("写 IMAP 命令失败: {e}"))?;
    s.get_mut().flush().ok();
    let mut out = String::new();
    loop {
        let mut line = String::new();
        let n = s
            .read_line(&mut line)
            .map_err(|e| format!("读 IMAP 回复失败: {e}"))?;
        if n == 0 {
            return Err("IMAP 连接被对端关闭".to_string());
        }
        if line.starts_with(tag) {
            return Ok(out);
        }
        // 行尾 {N} 表示后面紧跟 N 字节字面量。
        if let Some(open) = line.rfind('{') {
            if line.trim_end().ends_with('}') {
                let n: usize = line[open + 1..line.trim_end().len() - 1]
                    .trim()
                    .parse()
                    .unwrap_or(0);
                if n > 0 {
                    let mut buf = vec![0u8; n];
                    s.read_exact(&mut buf)
                        .map_err(|e| format!("读 IMAP 字面量失败: {e}"))?;
                    out.push_str(&String::from_utf8_lossy(&buf));
                    continue;
                }
            }
        }
        out.push_str(&line);
    }
}

// 把回信正文清理成用户新增的话：去掉 marker 行，再剥掉引用/转发/任务备注。
// 剥离规则与评论通道共用，见 ops::strip_reply_noise。
fn clean_reply(body: &str, marker: &str) -> String {
    let kept: Vec<&str> = body
        .lines()
        .filter(|l| !l.contains(marker) && !l.trim().is_empty())
        .collect();
    crate::ops::strip_reply_noise(&kept.join("\n"))
}

// 解析一封 RFC822 邮件：返回（解码后的主题，纯文本正文）。
pub fn parse_message(raw: &str) -> (String, String) {
    let (head, body) = split_headers(raw).unwrap_or((raw, ""));
    let subject = header(head, "subject")
        .map(|s| decode_rfc2047(&s))
        .unwrap_or_default();
    let ctype_raw = header(head, "content-type").unwrap_or_default();
    // 比较用小写，取 boundary 用原串——boundary 区分大小写，小写化后分隔符就对不上了。
    let ctype = ctype_raw.to_lowercase();
    let cte = header(head, "content-transfer-encoding")
        .unwrap_or_default()
        .to_lowercase();

    let text = if ctype.starts_with("multipart/") {
        let boundary = param(&ctype_raw, "boundary").unwrap_or_default();
        first_text_part(body, &boundary)
    } else {
        decode_body(body, &cte)
    };
    let text = if ctype.contains("text/html") || text.trim_start().starts_with('<') {
        strip_html(&text)
    } else {
        text
    };
    (subject, text)
}

// 按空行切「头 / 体」。
fn split_headers(s: &str) -> Option<(&str, &str)> {
    let i = s.find("\r\n\r\n").or_else(|| s.find("\n\n"))?;
    let sep = if s[i..].starts_with("\r\n\r\n") { 4 } else { 2 };
    Some((&s[..i], &s[i + sep..]))
}

fn header(head: &str, name: &str) -> Option<String> {
    // 支持折行（续行以空格/Tab 开头）。
    let mut cur: Option<String> = None;
    for line in head.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(c) = cur.as_mut() {
                c.push(' ');
                c.push_str(line.trim());
            }
            continue;
        }
        if let Some(c) = cur.take() {
            if c.to_lowercase().starts_with(&format!("{name}:")) {
                return Some(c[name.len() + 1..].trim().to_string());
            }
        }
        cur = Some(line.to_string());
    }
    if let Some(c) = cur {
        if c.to_lowercase().starts_with(&format!("{name}:")) {
            return Some(c[name.len() + 1..].trim().to_string());
        }
    }
    None
}

fn param(ctype: &str, key: &str) -> Option<String> {
    let idx = ctype.find(&format!("{key}="))?;
    let rest = &ctype[idx + key.len() + 1..];
    let rest = rest.trim_start_matches('"');
    let end = rest.find(['"', ';', ' ']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn decode_body(body: &str, cte: &str) -> String {
    if cte.contains("base64") {
        String::from_utf8_lossy(&b64_decode(body)).to_string()
    } else if cte.contains("quoted-printable") {
        String::from_utf8_lossy(&qp_decode(body)).to_string()
    } else {
        body.to_string()
    }
}

// multipart 里取第一段 text/plain；没有就退回整段（当 html 处理）。
fn first_text_part(body: &str, boundary: &str) -> String {
    if boundary.is_empty() {
        return body.to_string();
    }
    let sep = format!("--{boundary}");
    let mut best = String::new();
    for part in body.split(&sep) {
        // 结尾的 "--BND--" 被切开后只剩 "--"，跳过；空段也跳过。
        let trimmed = part.trim();
        if trimmed.is_empty() || trimmed == "--" {
            continue;
        }
        let Some((ph, pb)) = split_headers(part) else {
            continue;
        };
        let ct_raw = header(ph, "content-type").unwrap_or_default();
        let ct = ct_raw.to_lowercase();
        let cte = header(ph, "content-transfer-encoding")
            .unwrap_or_default()
            .to_lowercase();
        if ct.starts_with("multipart/") {
            let inner = param(&ct_raw, "boundary").unwrap_or_default();
            let t = first_text_part(pb, &inner);
            if !t.trim().is_empty() {
                return t;
            }
            continue;
        }
        let text = decode_body(pb, &cte);
        if ct.contains("text/plain") && !text.trim().is_empty() {
            return text;
        }
        if best.is_empty() && !text.trim().is_empty() {
            best = if ct.contains("text/html") {
                strip_html(&text)
            } else {
                text
            };
        }
    }
    best
}

pub fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut last_blank = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !last_blank {
                    out.push(' ');
                    last_blank = true;
                }
            }
            _ if in_tag => {}
            '\r' => {}
            c => {
                if c == '\n' {
                    if !last_blank {
                        out.push('\n');
                        last_blank = true;
                    }
                } else {
                    out.push(c);
                    last_blank = c == ' ';
                }
            }
        }
    }
    // 顺带把 &nbsp; 之类最常见的实体还原，够用即可。
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .trim()
        .to_string()
}

// RFC2047：=?UTF-8?B?xxx?= 或 =?UTF-8?Q?xxx?=
//
// 注意不能直接找第一个 "?=" 当结尾：Q 编码的正文里全是 "=XX"，
// 而 "=?UTF-8?Q?=E4=B8=AD?=" 里 charset 后面的 "?=" 会提前命中。
// 必须按 charset?encoding?payload?= 三段式切。
pub fn decode_rfc2047(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("=?") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let (Some(q1), Some(q2)) = (after.find('?'), after.find('?').and_then(|i| {
            after[i + 1..].find('?').map(|j| i + 1 + j)
        })) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let enc = after[q1 + 1..q2].to_uppercase();
        let payload_start = q2 + 1;
        let Some(end_rel) = after[payload_start..].find("?=") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let payload = &after[payload_start..payload_start + end_rel];
        let bytes = if enc == "B" {
            b64_decode(payload)
        } else {
            qp_decode(&payload.replace('_', " "))
        };
        out.push_str(&String::from_utf8_lossy(&bytes));
        rest = &after[payload_start + end_rel + 2..];
    }
    out.push_str(rest);
    out
}

// ---- 编码工具 ----

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Vec<u8> {
    let mut bits = 0u32;
    let mut nbits = 0u32;
    let mut out = Vec::new();
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue, // '=' / 换行 / 空白
        } as u32;
        bits = (bits << 6) | v;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    out
}

pub fn qp_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'=' {
            // 软换行：=\r\n 或 =\n，直接吞掉
            if i + 1 < b.len() && b[i + 1] == b'\n' {
                i += 2;
                continue;
            }
            if i + 2 < b.len() && b[i + 1] == b'\r' && b[i + 2] == b'\n' {
                i += 3;
                continue;
            }
            if i + 2 < b.len() {
                if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn wrap(s: &str, n: usize) -> Vec<String> {
    s.as_bytes()
        .chunks(n)
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip_handles_cjk() {
        for s in ["", "a", "ab", "abc", "客户问：能不能明天上线？"] {
            assert_eq!(b64_decode(&b64_encode(s.as_bytes())), s.as_bytes());
        }
    }

    #[test]
    fn quoted_printable_decodes_utf8_and_soft_breaks() {
        // 「中文」的 UTF-8 是 E4B8AD E69687
        let d = qp_decode("=E4=B8=AD=E6=96=87=\r\n ok");
        assert_eq!(String::from_utf8_lossy(&d), "中文 ok");
    }

    #[test]
    fn rfc2047_decodes_both_encodings() {
        assert_eq!(decode_rfc2047("=?UTF-8?B?5Lit5paH?="), "中文");
        assert_eq!(decode_rfc2047("=?UTF-8?Q?=E4=B8=AD?="), "中");
        assert_eq!(decode_rfc2047("Re: =?UTF-8?B?5Lit5paH?= 好"), "Re: 中文 好");
    }

    #[test]
    fn parses_plain_reply_with_marker() {
        let raw = "Subject: Re: [TB clm-1-0] 需要你确认\r\n\
                   Content-Type: text/plain; charset=UTF-8\r\n\
                   Content-Transfer-Encoding: base64\r\n\r\n";
        let body = b64_encode("可以，明天上线\n> 原问题\n".as_bytes());
        let (subject, text) = parse_message(&format!("{raw}{body}"));
        assert!(subject.contains("[TB clm-1-0]"), "主题要能读出来: {subject}");
        assert!(text.contains("可以，明天上线"), "正文要能解出来: {text}");
    }

    #[test]
    fn parses_multipart_alternative_prefers_plain_text() {
        let raw = "Subject: hi\r\n\
                   Content-Type: multipart/alternative; boundary=\"BND\"\r\n\r\n\
                   --BND\r\n\
                   Content-Type: text/plain; charset=UTF-8\r\n\r\n\
                   纯文本回复\r\n\
                   --BND\r\n\
                   Content-Type: text/html; charset=UTF-8\r\n\r\n\
                   <p>HTML 回复</p>\r\n\
                   --BND--\r\n";
        let (_, text) = parse_message(raw);
        assert!(text.contains("纯文本回复"), "应优先取 text/plain: {text}");
        assert!(!text.contains("HTML 回复"), "不该混进 html 段: {text}");
    }

    #[test]
    fn html_body_is_stripped() {
        let raw = "Subject: x\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n\
                   <div>答复：<b>同意</b></div>";
        let (_, text) = parse_message(raw);
        assert!(text.contains("答复"), "去标签后要留文字: {text}");
        assert!(text.contains("同意"));
        assert!(!text.contains('<'), "不该留标签: {text}");
    }

    #[test]
    fn clean_reply_drops_quoted_history_and_marker() {
        let body = "[TB clm-1-0] 收到\n可以，按方案二做\n\n> 之前的问题\n> 更多引用";
        let got = clean_reply(body, "[TB clm-1-0]");
        assert_eq!(got, "可以，按方案二做");
    }

    #[test]
    fn recipients_splits_and_filters() {
        assert_eq!(
            recipients("a@x.com; b@y.com,c@z.com  nonsense"),
            vec!["a@x.com", "b@y.com", "c@z.com"]
        );
        assert!(recipients("").is_empty());
    }

    // 发件人写法：纯地址 / 显示名 + 地址 都要能解析出真正的地址。
    // 2026-10-31 的 502 就是「把显示名当地址发」造成的。
    #[test]
    fn from_supports_display_name() {
        let (n, a) = split_from("www.3850@qq.com");
        assert_eq!(n, None);
        assert_eq!(a, "www.3850@qq.com");

        let (n, a) = split_from("works on teambition <www.3850@qq.com>");
        assert_eq!(n.as_deref(), Some("works on teambition"));
        assert_eq!(a, "www.3850@qq.com");

        let (n, a) = split_from("\"works on teambition\" <www.3850@qq.com>");
        assert_eq!(n.as_deref(), Some("works on teambition"));
        assert_eq!(a, "www.3850@qq.com");

        // 只有显示名：解析不出地址，send 会拦下来报错，而不是发出个 502。
        let (n, a) = split_from("works on teambition");
        assert_eq!(n, None);
        assert!(!a.contains('@'));
    }

    // From 头里的地址必须包在 <> 里，显示名走 RFC2047。
    #[test]
    fn message_from_header_uses_address_only() {
        let tos = vec!["a@b.com".to_string()];
        let m = build_message(
            Some("works on teambition"),
            "www.3850@qq.com",
            &tos,
            "主题",
            "正文",
        );
        assert!(
            m.contains("From: =?UTF-8?B?") && m.contains("<www.3850@qq.com>"),
            "显示名该走 RFC2047，地址该在尖括号里:\n{m}"
        );
        assert!(
            !m.contains("From: works on teambition"),
            "显示名不能裸着写进 From 头"
        );
    }
}
