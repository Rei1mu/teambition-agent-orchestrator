use crate::store::Store;
use crate::tb::Tb;

// 把 query 字符串做百分号编码。只覆盖常见字符。
fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

// Teambition 创建 User Token 的页面。扫码登录后创建，拿到 Token ID。
pub const TOKEN_PAGE: &str = "https://open.teambition.com/user-mcp";

pub fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

pub fn verify(store: &Store) -> Result<usize, String> {
    let mut tb = Tb::new(store.clone())?;
    tb.discover(false)?;
    Ok(tb.tools.len())
}

pub fn login(store: &Store, token: Option<String>) -> Result<(), String> {
    let mut cfg = store.load_config();

    if let Some(t) = token {
        let t = t.trim().to_string();
        if t.is_empty() {
            return Err("token 为空".to_string());
        }
        cfg.access_token = Some(t);
        store.save_config(&cfg)?;
        let n = verify(store)?;
        println!("登录成功。发现 {} 个 MCP 工具。", n);
        return Ok(());
    }

    let has_oauth = cfg.client_id.is_some()
        && cfg.client_secret.is_some()
        && cfg.authorize_url.is_some()
        && cfg.token_url.is_some();
    if has_oauth {
        return oauth_loopback(store);
    }

    // 没有 token 也没有 OAuth 配置：打开 Token 页面，手把手引导。
    println!("正在打开 Teambition 的 User Token 页面...");
    println!("浏览器没自动打开就手动访问: {TOKEN_PAGE}");
    println!();
    println!("创建 Token 的步骤:");
    println!("  1. 用钉钉扫码登录 Teambition");
    println!("  2. 点「创建 Token」按钮");
    println!("  3. 填 Token 名称、有效期(建议选 1 年)、选择使用企业");
    println!("  4. 点确认，再点「查看详情」复制 Token ID");
    println!("  5. 回到这里运行: tb login --token <刚才复制的Token>");
    open_browser(TOKEN_PAGE);
    Ok(())
}

pub fn logout(store: &Store) -> Result<(), String> {
    let mut cfg = store.load_config();
    cfg.access_token = None;
    store.save_config(&cfg)?;
    println!("已退出登录。");
    Ok(())
}

// 本地回环 OAuth。起 127.0.0.1 随机端口监听，浏览器授权后拿 code 换 token。
fn oauth_loopback(store: &Store) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let cfg = store.load_config();
    let client_id = cfg.client_id.clone().unwrap();
    let client_secret = cfg.client_secret.clone().unwrap();
    let authorize_url = cfg.authorize_url.clone().unwrap();
    let token_url = cfg.token_url.clone().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");

    let auth = format!(
        "{}?client_id={}&response_type=code&redirect_uri={}",
        authorize_url,
        enc(&client_id),
        enc(&redirect)
    );
    println!("请在浏览器完成授权: {auth}");
    open_browser(&auth);

    let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let code = req
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| {
            let q = path.split('?').nth(1)?;
            q.split('&')
                .find(|kv| kv.starts_with("code="))
                .map(|kv| kv[5..].to_string())
        })
        .ok_or("授权回调里没有 code")?;

    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body>Authorization complete. You can close this page.</body></html>",
    );
    drop(stream);

    let http = reqwest::blocking::Client::new();
    let resp = http
        .post(&token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
        ])
        .send()
        .map_err(|e| format!("换 token 失败: {e}"))?;
    let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or(format!("响应里没有 access_token: {v}"))?
        .to_string();

    let mut cfg = store.load_config();
    cfg.access_token = Some(access);
    if let Some(r) = v.get("refresh_token").and_then(|x| x.as_str()) {
        let _ = r; // 简化：不保存 refresh
    }
    store.save_config(&cfg)?;
    let n = verify(store)?;
    println!("登录成功。发现 {} 个 MCP 工具。", n);
    Ok(())
}
