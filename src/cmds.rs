use crate::config::*;
use crate::mail;
use crate::ops::*;
use crate::store::Store;
use crate::Cmd;
use clap::Parser;
use std::io::Read;
use std::io::Write;

// 参数小修正：把 `-off` 改写成 `--off`。
//
// 用户和 agent 都会顺手写单横线（`tba go -off "..."`），clap 会把它当成 `-o -f -f`
// 直接报「unexpected argument」。这里做一次无损改写，比让人去背双横线靠谱。
fn normalize_args(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    args.into_iter()
        .map(|a| {
            let s = a.to_string_lossy();
            if s == "-off" {
                return std::ffi::OsString::from("--off");
            }
            if let Some(rest) = s.strip_prefix("-off=") {
                return std::ffi::OsString::from(format!("--off={rest}"));
            }
            a
        })
        .collect()
}

// 自检：一条命令把「环境到底缺什么」全列出来。
//
// 为什么值得单独做：agent 排查「为什么跑不通」时会一个个试（which git / 看 token /
// 找 exe 路径 / 猜 data 在哪），每次都烧十几秒和一堆 token。这里一次全给。
fn doctor(store: &Store) -> String {
    let cfg = store.load_config();
    let mut s = String::new();

    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".to_string());
    s.push_str(&format!("exe:   {exe}\n"));
    s.push_str(&format!("版本:  {}\n", env!("CARGO_PKG_VERSION")));
    // PATH 里有没有 exe 所在目录：没有的话 agent 只能用绝对路径，`tba` 会报「找不到命令」。
    let exe_dir = std::path::Path::new(&exe)
        .parent()
        .map(|p| p.display().to_string().to_lowercase())
        .unwrap_or_default();
    let in_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(';')
        .any(|p| p.trim_end_matches('\\').to_lowercase() == exe_dir.trim_end_matches('\\'));
    s.push_str(&format!(
        "PATH:  {}\n",
        if in_path {
            "已包含 exe 目录（新终端可直接 tba）"
        } else {
            "不含 exe 目录（要么用绝对路径，要么重装/重启终端）"
        }
    ));

    s.push_str(&format!(
        "令牌:  {}\n",
        match cfg.token() {
            Some(t) => format!("有（{}…，{} 位）", &t[..t.len().min(6)], t.len()),
            None => "没有（先 tba login --token <令牌>）".to_string(),
        }
    ));
    s.push_str(&format!("MCP:   {}\n", cfg.mcp_url));
    s.push_str(&format!("数据:  {}\n", store.root.display()));

    // git / GitButler：这两个是通用能力的地基，缺了 agent 会以为是自己的问题。
    match crate::gitbutler::git_available() {
        Some(v) => s.push_str(&format!("git:   {v}\n")),
        None => s.push_str("git:   没装（提交推送全不可用）\n"),
    }
    let gb = crate::gitbutler::detect(store);
    s.push_str(&format!(
        "but:   {}\n",
        match (&gb.version, gb.found) {
            (Some(v), _) => format!("{v}（{}）", gb.source.map(|x| x.label()).unwrap_or("?")),
            (None, true) => "装了但取不到版本".to_string(),
            (None, false) => "没装（tba gitbutler install 可装到运行目录）".to_string(),
        }
    ));
    if let Ok(root) = crate::git::repo_root() {
        s.push_str(&format!(
            "仓库:  {}（GitButler 项目: {}）\n",
            root.display(),
            if crate::git::is_gb_project(&root) { "是" } else { "否" }
        ));
    } else {
        s.push_str("仓库:  当前目录不在 git 仓库里\n");
    }
    // GitHub token：tba git create 唯一的外部依赖。只查环境变量，
    // 不跑 `git credential fill`（可能弹凭据窗）；git 凭据那条由命令自己兜底。
    let gh_env = ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .find(|k| std::env::var(k).map(|v| !v.trim().is_empty()).unwrap_or(false));
    s.push_str(&format!(
        "GitHub: {}\n",
        match gh_env {
            Some(k) => format!("{k} 已设（tba git create 可建仓库）"),
            None => "没设 GH_TOKEN/GITHUB_TOKEN；若 `git push` 能用，tba git create 会读 git 凭据".to_string(),
        }
    ));

    // 字体：中文界面缺字体就会满屏口口，这条能直接定位。
    let fonts: Vec<&str> = [
        ("黑体", "C:/Windows/Fonts/simhei.ttf"),
        ("雅黑", "C:/Windows/Fonts/msyh.ttc"),
        ("等线", "C:/Windows/Fonts/Deng.ttf"),
        ("宋体", "C:/Windows/Fonts/simsun.ttc"),
    ]
    .iter()
    .filter(|(_, p)| std::path::Path::new(p).exists())
    .map(|(n, _)| *n)
    .collect();
    s.push_str(&format!(
        "字体:  {}\n",
        if fonts.is_empty() {
            "一个中文字体都没找到（界面会显示方块）".to_string()
        } else {
            fonts.join(" / ")
        }
    ));

    // 等待参数：这几个值配错会让「等回复」提前返回，是常见坑。
    s.push_str(&format!(
        "轮询:  ask 超时 {}s / 间隔 {}s / 邮件 {}s\n",
        cfg.wait_timeout_secs, cfg.wait_interval_secs, cfg.mail_poll_secs
    ));
    s.trim_end().to_string()
}


// 从正文首个非空行推断任务标题：`# xxx` 去掉 #，普通句子取前 60 字。
fn first_heading(body: &str) -> Option<String> {
    let first = body.lines().find(|l| !l.trim().is_empty())?;
    let t = first.trim();
    let h = t.strip_prefix("# ").unwrap_or(t).trim();
    if h.is_empty() {
        None
    } else {
        Some(h.chars().take(60).collect())
    }
}

// 解析 --project：给了就用；没给走 .tba/project 绑定。
// 两者都没有就报错——绝不猜项目，猜错会把任务领到别的项目里。
fn resolve_project(project: Option<&str>) -> Result<String, String> {
    if let Some(p) = project.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(p.to_string());
    }
    match read_project_binding() {
        Some(b) => Ok(b),
        None => Err(
            "没指定项目，当前目录也没绑定。先 `tba ensure-project --dry-run` 看候选项目名、\
             让用户挑一个再 `--name`，或加 --project <id>"
                .to_string(),
        ),
    }
}

// 每条回复打印给 agent 看。常驻监听的 stdout 被重定向到 feed 文件，
// 前台等待者按偏移读新增内容，读到就退出唤醒会话。
// 返回 true = 用户回 1 收尾，监听到此为止。
fn print_outcome(o: &WaitOutcome, cfg: &Config) -> bool {
    match o {
        WaitOutcome::Replied {
            claim: c,
            replies,
            action,
            body,
            waited_secs,
        } => {
            if *action == ReplyAction::Closed {
                println!(
                    "收到回复 1（等了 {waited_secs} 秒）。任务「{}」已收尾：「{}」+ 看板列「{}」。",
                    c.task_name, STAGE_DONE, cfg.tasklist_done
                );
            } else {
                println!(
                    "收到回复（等了 {waited_secs} 秒）。任务「{}」已回到「{}」+ 看板列「{}」。",
                    c.task_name, STAGE_DOING, cfg.tasklist_claim
                );
            }
            // 结构化标记：只有真收到用户回复才打这行。`ask` 前台 exit 0 只是起了监听，
            // 绝不能当收尾确认。agent 看 action 决定下一步，不用自己解析中文。
            let num = action.num().map(|n| n.to_string()).unwrap_or_else(|| "-".into());
            println!("USER_REPLY=1");
            println!("reply={num} action={} text={:?}", action.tag(), body);
            println!("\n---- 你的回复 ----");
            for r in replies {
                println!("[用户回复] [{}] {}\n{}", r.created, r.author, r.text);
            }
            println!();
            match action {
                ReplyAction::Closed => {
                    println!("已收尾，停。别再跑 done/close。");
                }
                ReplyAction::Summarize => {
                    println!(
                        "用户要求先把这次的经验写进项目记忆：\n  tba memory --append --section 踩坑 \"...\"\n写完再进待回复：\n  tba ask {} --done",
                        c.claim_id
                    );
                }
                ReplyAction::Custom(n) => {
                    println!("用户说有问题，选了你自定义的选项 {n}，按你在问题里写的语义执行。干完汇报。");
                    if !c.question.trim().is_empty() {
                        println!("\n---- 本次问题（选项 {n} 的含义） ----\n{}", c.question);
                    }
                }
                ReplyAction::Free => {
                    println!("用户没回有效数字（含废弃的 0），按原文理解。干完汇报。");
                }
            }
            let _ = std::io::stdout().flush();
            *action == ReplyAction::Closed
        }
        WaitOutcome::Noted {
            claim: c,
            replies,
            waited_secs,
        } => {
            // 执行中收到的用户原话。任务状态一个字没动，交给 agent 判断。
            println!(
                "常驻监听 {} 秒后，收到你在【执行中】发来的回复（任务「{}」状态未改动）。",
                waited_secs, c.task_name
            );
            let num = replies
                .iter()
                .rev()
                .find_map(|r| parse_reply_num(r))
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into());
            println!("USER_REPLY=1");
            println!("reply={num} action=noted");
            println!("\n---- 你的回复 ----");
            for r in replies {
                println!("[用户回复] {r}\n");
            }
            println!(
                "已记进 `tba events`。常驻监听继续挂着（执行中也不停，直到你回 1）；\n\
                 处理完这一轮再挂一条 `tba wait {}` 等下一条回复，别等干完才 ask。",
                c.claim_id
            );
            let _ = std::io::stdout().flush();
            false
        }
        _ => true,
    }
}

// 常驻监听本体（`--listen`）。只有用户回 1 / 别处收尾才退出；
// 其余回复交给 emit 打印后继续轮询，执行中窗口的发言也不会漏。
//
// 退出码约定（agent 靠这个决定下一步，不要改）：
//   正常返回 = 监听结束（已收尾），正文已打印
//   2        = --once 单轮超时（默认 540 秒），接着跑 tba wait 即可
//   3        = 用户在网页上手动收尾了，停
fn run_wait_block(
    store: &Store,
    claim_id: &str,
    timeout: Option<u64>,
    interval: Option<u64>,
    forever: bool,
    listen: bool,
) -> Result<(), String> {
    let cfg = store.load_config();
    // 命令行参数优先，没给就用配置里的（data/config.toml，GUI 设置页也能改）。
    // 无论来源都夹一次下限：0 或 3 秒会让轮询变成忙等，把接口打爆。
    let t = timeout
        .unwrap_or(cfg.wait_timeout_secs)
        .max(crate::config::MIN_WAIT_TIMEOUT_SECS);
    let iv = interval
        .unwrap_or(cfg.wait_interval_secs)
        .max(crate::config::MIN_WAIT_INTERVAL_SECS);
    let outcome = wait(store, claim_id, t, iv, forever, |o| {
        let closed = print_outcome(o, &cfg);
        // 常驻监听只有收尾才停；--once 老路径收到一条就返回。
        !listen || closed
    })?;
    match outcome {
        // 回复已由 emit 逐条打印到 feed，这里不重复打。
        WaitOutcome::Replied { .. } | WaitOutcome::Noted { .. } => Ok(()),
        WaitOutcome::Timeout {
            claim: c,
            waited_secs,
        } => {
            // 进行中态（等手动收尾）vs 待回复态（等评论），提示要分开。
            let where_ = if c.state == "waiting" {
                format!("「{}」+ 看板列「{}」", STAGE_WAIT, cfg.tasklist_wait)
            } else {
                format!("「{}」+ 看板列「{}」", STAGE_DOING, cfg.tasklist_claim)
            };
            println!(
                "等了 {waited_secs} 秒还没回复。任务「{}」仍在{where_}，球在你那边。",
                c.task_name
            );
            println!(
                "继续等就再跑一次: tba wait {}\n\
                 （退出码 2 = --once 单轮到点，不是失败；常驻模式不会超时。）",
                c.claim_id
            );
            // 退出码 2 = 超时（没回复/没收尾），agent 据此再等一轮。
            std::process::exit(2);
        }
        WaitOutcome::Gone {
            claim: c,
            state,
            waited_secs,
        } => {
            println!(
                "等了 {waited_secs} 秒，任务「{}」在别处被收尾了（{state}），不再等。",
                c.task_name
            );
            std::process::exit(3);
        }
    }
}

// ask / wait 拉起一个 detached 常驻监听（`tba wait <claim> --listen`）。
// 它的 stdout 追加到 feed 文件：每条回复摘要都落盘，前台等待者按偏移读新增。
// 监听不随回复退出，父进程被杀也不受影响（新进程组 + DETACHED）。
fn spawn_detached_listener(
    claim_id: &str,
    feed_path: &std::path::Path,
) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("取当前 exe 路径失败: {e}"))?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("wait").arg(claim_id).arg("--listen").stdin(std::process::Stdio::null());
    if let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(feed_path)
    {
        cmd.stdout(std::process::Stdio::from(f));
    } else {
        cmd.stdout(std::process::Stdio::null());
    }
    // stderr → null：轮询期间每 300s 的「已等 N 秒」心跳打这里，不污染 feed。
    cmd.stderr(std::process::Stdio::null());
    // 新进程组：脱离 ask 的进程组，ask 被 SIGTERM 不会连带杀到它。
    // DETACHED_PROCESS：不共享 ask 的控制台，关窗口也不死。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0200 | 0x0000_0008); // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let _ = cmd.process_group(0);
    }
    cmd.spawn().map_err(|e| format!("spawn detached listener 失败: {e}"))?;
    Ok(())
}

// 保证有且只有一个常驻监听在跑。活着的监听每轮刷 waits/<claim>.wait。
fn ensure_listener(store: &Store, claim_id: &str) -> Result<(), String> {
    if store.wait_age(claim_id).map_or(false, |age| (0..30).contains(&age)) {
        return Ok(());
    }
    let feed = store.waits_dir().join(format!("{claim_id}.feed"));
    spawn_detached_listener(claim_id, &feed)?;
    // 占位：新进程还没来得及 touch 之前，别让并发的 ask/wait 再拉一个。
    store.wait_touch(claim_id);
    Ok(())
}

// 前台等待者：读到 feed 新增就打印并退出（退出即唤醒会话）。
// 监听本体不退出，要靠这条命令把「有新回复」变成一次 task-notification。
fn wait_for_feed(store: &Store, claim_id: &str) -> Result<(), String> {
    let dir = store.waits_dir();
    let feed = dir.join(format!("{claim_id}.feed"));
    let cursor = dir.join(format!("{claim_id}.cursor"));
    loop {
        let at: usize = std::fs::read_to_string(&cursor)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if let Ok(bytes) = std::fs::read(&feed) {
            if bytes.len() > at {
                print!("{}", String::from_utf8_lossy(&bytes[at..]));
                let _ = std::io::stdout().flush();
                let _ = std::fs::write(&cursor, bytes.len().to_string());
                return Ok(());
            }
        }
        // 任务被收尾就停，别死等一个已经没意义的回复。
        if store.get_claim(claim_id).map_or(false, |c| !c.is_active()) {
            println!("任务已收尾，停。");
            return Ok(());
        }
        // 监听意外挂了就自愈重拉，任务还在进行中就别断。
        if store.wait_age(claim_id).map_or(true, |age| age >= 60) {
            let _ = ensure_listener(store, claim_id);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

// 通用能力的命令行入口。全是本地文件操作，不联网、不需要 token。
//
// 输出原则跟项目 skill 一致：list 只给「名字 + 一句话」，正文靠 show 或直接给名字
// 单独读——免得 agent 为了找一条命令把五份长文全塞进上下文。
fn cap_cmd(
    store: &Store,
    action: &str,
    name: Option<&str>,
    summary: Option<&str>,
    summary_en: Option<&str>,
    body: Option<&str>,
    off: bool,
    always: Option<bool>,
) -> Result<String, String> {
    use crate::caps::{self, Cap};

    let read_body = |src: Option<&str>| -> Result<Option<String>, String> {
        match src {
            None => Ok(None),
            Some("-") => {
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| format!("读 stdin 失败: {e}"))?;
                Ok(Some(s))
            }
            Some(p) => std::fs::read_to_string(p)
                .map(Some)
                .map_err(|e| format!("读 {p} 失败: {e}")),
        }
    };

    match action {
        "list" | "ls" => {
            let all = caps::list(store);
            if all.is_empty() {
                return Ok(
                    "还没有通用能力。用 `tba cap add <名字> --summary \"...\" --body <文件>` 加一条，\n\
                     或 `tba cap restore` 把内置的恢复回来。"
                        .to_string(),
                );
            }
            let on = all.iter().filter(|c| c.on).count();
            let w = all
                .iter()
                .map(|c| c.name.chars().count())
                .max()
                .unwrap_or(4)
                .max(4);
            let mut out = format!("通用能力 {} 条（启用 {}）\n", all.len(), on);
            for c in &all {
                out.push_str(&format!(
                    "  [{}] {:w$}  {}{}{}\n",
                    if c.on { "开" } else { "关" },
                    c.name,
                    if c.always { "【常驻】" } else { "" },
                    c.summary,
                    if c.builtin { "" } else { "（自定义）" },
                    w = w
                ));
            }
            out.push_str("\n读正文: tba cap <名字>");
            out.push_str("\n常驻 = 开工文案里会标出来（给动手前就得知道的红线用）");
            out.push_str(&format!("\n文件: {}", caps::path(store).display()));
            Ok(out)
        }
        "path" => Ok(caps::path(store).display().to_string()),
        "add" | "set" | "edit" => {
            let n = name.ok_or("要指定能力名：tba cap add <名字> --summary \"...\"")?;
            caps::check_name(n)?;
            let old = caps::get(store, n);
            let new_body = read_body(body)?;
            let existed = old.is_some();
            let cap = Cap {
                name: n.trim().to_string(),
                summary: summary
                    .map(|s| s.to_string())
                    .or_else(|| old.as_ref().map(|c| c.summary.clone()))
                    .unwrap_or_default(),
                summary_en: summary_en
                    .map(|s| s.to_string())
                    .or_else(|| old.as_ref().map(|c| c.summary_en.clone()))
                    .unwrap_or_default(),
                on: !off,
                builtin: old.as_ref().map(|c| c.builtin).unwrap_or(false),
                // 不给 --always/--no-always 就保留原值（新建默认 false）。
                always: always
                    .or_else(|| old.as_ref().map(|c| c.always))
                    .unwrap_or(false),
                body: new_body
                    .or_else(|| old.as_ref().map(|c| c.body.clone()))
                    .unwrap_or_default(),
            };
            let msg = caps::upsert(store, cap)?;
            // 编辑时 upsert 保留原开关（用户可能特意关过），要关得显式说。
            if existed && off {
                caps::set_on(store, n, false)?;
            }
            Ok(msg)
        }
        "rm" | "del" | "remove" => caps::remove(
            store,
            name.ok_or("要指定能力名：tba cap rm <名字>")?,
        ),
        "on" | "off" => caps::set_on(
            store,
            name.ok_or("要指定能力名：tba cap on|off <名字>")?,
            action == "on",
        ),
        "restore" => caps::restore(store, name),
        "show" | "get" => {
            let n = name.ok_or("要指定能力名：tba cap show <名字>")?;
            let c = caps::get(store, n)
                .ok_or_else(|| format!("没有这个能力: {n}（tba cap list 看有哪些）"))?;
            Ok(c.body)
        }
        other => {
            // `tba cap shot` 直接读正文：少打一个 show，agent 更省事。
            if name.is_none() {
                if let Some(c) = caps::get(store, other) {
                    return Ok(c.body);
                }
            }
            Err(format!(
                "未知动作: {other}（可选 list | show | add | edit | rm | on | off | restore | path，\
                 或直接给能力名）"
            ))
        }
    }
}

// `ensure-project --dry-run` / `tba create --dry`：只报候选，不建、不写绑定。
//
// 目的很具体：建项目前得先知道这个仓库该叫什么。目录名经常不可靠
// （可能叫 code / tmp / TBHub），所以扫 git remote、构建清单、子目录里的项目，
// 把候选连同来源列出来，由用户拍板。
// 扫与标注共用 create::scan，窗口「接入项目」页看到的是同一份数据。
fn print_name_candidates(store: &Store, given: Option<&str>, project: Option<&str>) {
    let dir = std::env::current_dir().unwrap_or_default();
    println!("扫描目录: {}", dir.display());
    let rows = crate::create::scan(store, &dir, given);
    if rows.is_empty() {
        println!("没找到可用的名字线索：没有 git remote，也没有 Cargo.toml / package.json 这类清单。");
        println!("直接问用户要一个项目名，再跑 `tba create --name \"<名字>\" --go`。");
        return;
    }
    if let Some(p) = project.map(str::trim).filter(|s| !s.is_empty()) {
        println!("指定项目 id: {p}（dry-run 不校验）");
    }
    println!("\n候选项目名（把这份原样交给用户拍板，不要自己挑）：");
    print_name_rows(&rows);
    println!("\n用户选定后：tba create --name \"<选定的名字>\" --go");
    println!("（--go 会顺手写 MCP 配置并当场领一条任务；不加就只建项目）");
    println!("以上只是列候选：没有创建项目，也没有改本地绑定。");
}

fn print_name_rows(rows: &[crate::create::Row]) {
    for (i, r) in rows.iter().enumerate() {
        println!("  {}. {}\t来源: {}\t{}", i + 1, r.name, r.source, r.mark);
    }
}

/// `tba create` 的交互拍板。返回用户选中的项目名。
/// 非交互（agent 管道调用、stdin 不是终端）时不阻塞，直接报错让 agent 去问用户。
fn pick_name_interactive(rows: &[crate::create::Row]) -> Result<String, String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(
            "没有终端可以选号。把上面的候选原样交给用户拍板，再跑 \
             `tba create --name \"<选定的名字>\" --go`。没有创建项目。"
                .to_string(),
        );
    }
    print!("\n输入编号（1..{}）或直接输入项目名，回车 = 取消: ", rows.len());
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("读取输入失败: {e}"))?;
    let t = line.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("q") {
        return Err("已取消，没有创建项目。".to_string());
    }
    if let Ok(n) = t.parse::<usize>() {
        if n == 0 {
            return Err(format!("编号从 1 开始（1..{}），没有创建项目。", rows.len()));
        }
        return rows
            .get(n - 1)
            .map(|r| r.name.clone())
            .ok_or_else(|| format!("编号超出范围（1..{}），没有创建项目。", rows.len()));
    }
    Ok(t.to_string())
}

/// 给别的 agent 客户端手贴用的片段。command 同样用 exe 绝对路径：
/// 宿主进程常常没刷新 PATH，裸 `tba.exe` 会起不来。
/// 走 serde_json 生成，Windows 路径里的反斜杠才会正确转义。
fn mcp_snippet() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "tba": { "command": mcp_command(), "args": ["serve"] }
        }
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

/// MCP 配置里的 command 用装在正式位置的 exe 绝对路径。
/// 从 cargo target 跑开发版时别把调试 exe 写进宿主配置。
fn mcp_command() -> String {
    match std::env::current_exe() {
        Ok(p) => {
            let in_target = p
                .parent()
                .and_then(|d| d.file_name())
                .map(|n| n == "debug" || n == "release")
                .unwrap_or(false);
            if !in_target {
                return p.display().to_string();
            }
            if let Ok(home) = std::env::var("USERPROFILE") {
                let installed = std::path::Path::new(&home).join("tba").join("tba.exe");
                if installed.exists() {
                    return installed.display().to_string();
                }
            }
            p.display().to_string()
        }
        Err(_) => "tba.exe".to_string(),
    }
}

/// 把 tba 合并进 WorkBuddy 的 mcp.json。只动 mcpServers.tba，别的不碰。
fn mcp_install() -> Result<String, String> {
    let home = std::env::var("USERPROFILE")
        .map(std::path::PathBuf::from)
        .map_err(|e| format!("拿不到 USERPROFILE: {e}"))?;
    let path = home.join(".workbuddy-ai").join("mcp.json");
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut root: serde_json::Value = if old.trim().is_empty() {
        serde_json::json!({ "mcpServers": {} })
    } else {
        serde_json::from_str(&old).map_err(|e| format!("{} 不是合法 JSON: {e}", path.display()))?
    };
    if !root.is_object() {
        return Err(format!("{} 顶层不是对象", path.display()));
    }
    let servers = root
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        return Err(format!("{} 的 mcpServers 不是对象", path.display()));
    }
    servers.as_object_mut().unwrap().insert(
        "tba".to_string(),
        serde_json::json!({ "command": mcp_command(), "args": ["serve"] }),
    );
    std::fs::create_dir_all(path.parent().unwrap())
        .map_err(|e| format!("建目录失败: {e}"))?;
    let text = serde_json::to_string_pretty(&root).unwrap_or_default();
    std::fs::write(&path, text).map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
    Ok(format!(
        "已写入 {}（command 是 exe 绝对路径，不依赖 PATH 刷新）\n\
         MCP 对【当前会话】不生效：下次会话或重载后，在连接器管理页找到 tba 点「信任」启用。\n\
         其他 agent 的配置片段：跑 `tba mcp` 查看。",
        path.display()
    ))
}

/// 起双击用的窗口（GUI 子系统，不会带出控制台黑框）。
fn launch_gui(exe: &str) -> Result<String, String> {
    let gui = std::path::Path::new(exe)
        .parent()
        .ok_or_else(|| "取不到 exe 所在目录".to_string())?
        .join("tbagui.exe");
    if !gui.exists() {
        return Err(format!("找不到 {}（安装包不完整，重装 tba）", gui.display()));
    }
    std::process::Command::new(&gui)
        .spawn()
        .map_err(|e| format!("起窗口失败: {e}"))?;
    Ok(gui.display().to_string())
}

/// 第一次会话的设备接入：只装/查，不碰项目和任务队列。
/// 状态行前缀固定，agent 照着读就行，不用猜哪句是人话哪句是结论。
/// 刻意不联网：token 还没填的时候联网只会报一堆无关错误。
fn onboard(store: &Store) -> Result<(), String> {
    let cfg = store.load_config();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".to_string());
    let cwd = std::env::current_dir().unwrap_or_default();
    let bound = cwd.join(".tba").join("project").exists();

    println!("[tba onboard] 设备接入（第一次会话）");
    println!("EXE       {exe}");
    println!("VERSION   {}", env!("CARGO_PKG_VERSION"));
    println!(
        "TOKEN     {}",
        if cfg.token().is_some() { "OK" } else { "MISSING" }
    );

    if cfg.token().is_none() {
        match launch_gui(&exe) {
            Ok(p) => {
                println!("GUI       LAUNCHED {p}");
                println!("\n请在打开的 TBA 窗口里自己填 Teambition token 并保存。\n\
                         不要代填、不要去改 data/config.toml。填完本次会话就停。");
            }
            Err(e) => {
                println!("GUI       FAILED {e}");
                println!("\n窗口没起来：把上面的错误原样报给用户，让他重装 tba 或手动打开 tbagui.exe。\n\
                         不要代填 token、不要改 data/config.toml。本次会话到此为止。");
            }
        }
    } else {
        println!("GUI       NOT_NEEDED");
    }

    println!("WORKSPACE {}", cwd.display());
    println!("BINDING   {}", if bound { "OK" } else { "NONE" });

    // 沿用 C 决策：tba 只自动写 WorkBuddy AI 一家，别家由当前客户端的 agent 自己合并片段。
    match mcp_install() {
        Ok(msg) => println!("\nMCP_AUTO  WorkBuddy AI 已就绪\n{msg}"),
        Err(e) => println!("\nMCP_AUTO  SKIP（{e}）"),
    }
    println!("\n===== MCP 片段（其他客户端合并 mcpServers.tba 这一项） =====");
    println!("{}", mcp_snippet());

    println!("\n===== 第一次会话到此为止 =====");
    println!("不建项目、不领任务、不读 .tba/。");
    println!("把上面的 tba 项合并进你自己客户端的 MCP 配置：先备份、只改这一项、读回校验。");
    println!("然后回报：改动位置 + 需要用户在连接器管理页点「信任」。");
    println!("MCP 只对【下一次会话】生效，本次会话用命令行即可。");
    println!("第二次会话用户说「开工 / tba go」时，直接跑不带参数的 `tba`：");
    println!("  有项目空间就领任务；没有就列候选名交用户拍板，再 `tba create --name \"<选定的>\" --go`。");
    Ok(())
}

/// 「去队列领一条任务 + 打开工文案」。
/// Cmd::Go 和自举一条龙共用，避免两边行为漂移。
fn claim_and_print(
    store: &Store,
    project: Option<&str>,
    lang: Lang,
    write_entry: bool,
    guide_tasklists: bool,
) -> Result<(), String> {
    use std::io::Write;
    match go(store, project, lang, write_entry, guide_tasklists)? {
        GoOutcome::NoBinding { cwd } => {
            println!("NO_BINDING {cwd}");
            eprintln!("当前目录没有绑定 Teambition 项目（缺 .tba/project）。停，不领任务。");
            eprintln!(
                "要接入：跑 `tba`（不带参数）扫候选项目名，把候选交给用户挑，\
                 再 `tba create --name \"<选定的名字>\" --go`"
            );
            let _ = std::io::stdout().flush();
            std::process::exit(3);
        }
        GoOutcome::NoProject { id, name } => {
            if id.is_empty() {
                println!("NO_PROJECT {name}");
                eprintln!("当前目录按名字「{name}」在 Teambition 上没找到项目（或撞了多个同名）。");
            } else {
                println!("NO_PROJECT {id}");
                eprintln!("本地绑定指向的项目「{id}」在 Teambition 里查不到（可能被删了）。");
            }
            // 「绑定还在、线上项目没了」也算没有对应空间：别把 agent 卡在
            // ensure-project 上，按工作区重新列候选，交用户拍板后走 create --go。
            eprintln!("没有对应的 Teambition 项目空间。tba 不会自己建：项目名必须用户拍板。");
            eprintln!("按工作区重新接一次，候选如下：");
            // 这里只列名字线索，纯本地。别用 create::scan：它会再开一个 Ctx 联网
            // 核对同名项目 —— go() 刚查完项目列表才走到这，再握手一次就是白等一分钟。
            let dir = std::env::current_dir().unwrap_or_default();
            let rows: Vec<crate::create::Row> = crate::naming::candidates(&dir)
                .into_iter()
                .map(|c| crate::create::Row {
                    name: c.name,
                    source: c.source,
                    mark: "本地线索，未联网核对".to_string(),
                })
                .collect();
            if rows.is_empty() {
                eprintln!("没扫到候选名：直接问用户要一个项目名。");
            } else {
                eprintln!("候选项目名（原样交给用户拍板，不要自己挑）：");
                print_name_rows(&rows);
            }
            eprintln!(
                "用户点头后只跑 `tba create --name \"<选定的>\" --go`：复用/新建项目并重写本地绑定。"
            );
            eprintln!(
                "建完会打印项目链接，并自动补齐 未完成/进行中/待回复/已解决/已完成 五个看板列（老数字列会自动改名）；补不动才提示手动建。"
            );
            let _ = std::io::stdout().flush();
            std::process::exit(4);
        }
        GoOutcome::Empty { project_name } => {
            println!("EMPTY {project_name}");
            eprintln!("队列空：列「{STAGE_TODO}」没有可领任务。停。");
        }
        GoOutcome::Claimed { text, entry, .. } => {
            print!("{text}");
            if let Some(p) = entry {
                eprintln!("已补工作空间入口: {}", p.display());
            }
        }
        GoOutcome::Blocked { msg } => {
            println!("BLOCKED");
            eprintln!("{msg}");
            let _ = std::io::stdout().flush();
            // 退出码 5 = 手上有没人接的「待回复」，先处理掉再来领。
            std::process::exit(5);
        }
    }
    Ok(())
}

/// 自举下半段：名字已经由用户拍板，一条命令做完 create + MCP + 领任务。
/// 顺序上先写 MCP 再领任务：领任务可能 process::exit，得先把提示打出来。
fn finish_bootstrap(
    store: &Store,
    dir: &std::path::Path,
    name: &str,
    project: Option<&str>,
    lang: Lang,
) -> Result<(), String> {
    let r = crate::create::create(store, dir, name, project, lang)?;
    println!(
        "\n项目就绪: {}\t{}{}",
        r.project_id,
        r.project_name,
        if r.created { "（新建）" } else { "（复用）" }
    );
    println!("工作流状态: 7 个已补齐");
    println!("本地绑定: {}", r.binding.display());
    if let Some(p) = &r.entry {
        println!("AGENTS.md 入口: {}", p.display());
    }
    // 建/复用完就把项目链接和看板列结果给人。create() 已经自动补过 5 个中文列，
    // 这里用它的结果，不再让 go() 查第二遍（guide_tasklists=false）。
    print!(
        "{}",
        crate::ops::tasklist_guidance(&r.project_id, &r.tasklists, true)
    );

    // MCP 只是给「下一次会话」准备：写失败不能挡住接入和领任务。
    match mcp_install() {
        Ok(msg) => println!("\n[MCP] {msg}"),
        Err(e) => eprintln!(
            "[MCP] 写配置失败（不影响接入和领任务）：{e}\n\
             下次跑 `tba mcp --install` 可重试；其他客户端用 `tba mcp` 看片段。"
        ),
    }

    println!("\n===== 领任务 =====");
    // 用刚建/复用出来的项目 id，别让 go() 去读 cwd 的 .tba/project：
    // `--dir <别的目录>` 时绑定写在 dir，进程 cwd 里根本没有。
    // create() 已经在 dir 写过入口文件，这里 write_entry=false，别再往 cwd 补一份。
    claim_and_print(store, Some(r.project_id.as_str()), lang, false, false)
}

/// 无参数 + 没有 .tba/project 时的一次性自举卡。
/// 交互终端：就地选号，一次做完 create + MCP + 领任务。
/// agent（stdin 不是终端）：候选原样交给用户拍板，给一条收尾命令，绝不自己起名。
fn auto_bootstrap(store: &Store) -> Result<(), String> {
    use std::io::IsTerminal;
    println!(
        "tba 是本机已安装的命令行工具（在 PATH 里）。直接执行它。\n\
         不要为了找它去搜 exe、不要验证路径。跑不通就把错误原样报给用户。\n"
    );
    let dir = std::env::current_dir().unwrap_or_default();
    println!("当前目录 {} 还没接入 Teambition 项目。\n", dir.display());
    let rows = crate::create::scan(store, &dir, None);
    if rows.is_empty() {
        println!("没扫到候选项目名。把情况交给用户，问一个名字，再只跑这一条：");
        println!("  tba create --name \"<用户给的名字>\" --go");
        println!("（--go = 建项目 + 写 MCP 配置 + 当场领一条任务）");
        return Ok(());
    }
    println!("候选项目名（把这份原样交给用户拍板，不要自己挑）：");
    print_name_rows(&rows);

    // 人自己在终端敲：就地选号，一次做完，不用再记第二条命令。
    if std::io::stdin().is_terminal() {
        let picked = pick_name_interactive(&rows)?;
        println!("\n已选项目名: {picked}");
        return finish_bootstrap(store, &dir, &picked, None, Lang::Zh);
    }

    // agent 场景：不能替用户拍板，给一条收尾命令把剩下的全做完。
    println!("\n用户选定后只跑这一条，剩下的全部做完：");
    println!("  tba create --name \"<选定的名字>\" --go");
    println!("--go = 建/复用项目 + 补 7 状态 + 写绑定 + 写 MCP 配置 + 当场领一条任务。");
    println!("MCP 只对下一次会话生效；本次会话照样能用命令行干活。");
    Ok(())
}

pub fn run() -> Result<(), String> {
    let store = Store::new();
    store.ensure_dirs()?;
    let cli = crate::Cli::parse_from(normalize_args(std::env::args_os().collect()));

    // 任何命令启动都顺带给「本会话占着的 running 任务」刷一次心跳：
    // 会话还活着（还在跑 tb）就不该被当成僵尸回收，长构建期间尤其需要。
    // sweep / hello 除外——sweep 是来回收的，先续命就回收不掉了。
    // onboard 也除外：第一次会话只做设备接入，不该碰任务队列的任何状态。
    if !matches!(
        &cli.cmd,
        Some(Cmd::Sweep { .. }) | Some(Cmd::Hello { .. }) | Some(Cmd::Onboard)
    ) {
        store.beat_agent(&current_agent());
    }
    // 不带子命令 = 零参数自举。陌生 agent 只要会敲 `tba` 这一个词就能开工：
    //   - 当前目录有 .tba/project -> 直接当 go 领任务
    //   - 没有 -> 列候选交用户拍板；交互终端就地选号后一次做完，
    //            agent（非终端）拿到一条 `tba create --name X --go` 收尾
    // 双击窗口由 tbagui.exe 承担，tba.exe 无参数不再开 GUI。
    let cmd = match cli.cmd {
        Some(c) => c,
        None => {
            if crate::ops::read_project_binding().is_some() {
                Cmd::Go {
                    project: None,
                    lang: "zh".to_string(),
                    no_entry: false,
                    off: false,
                    task: Vec::new(),
                }
            } else {
                return auto_bootstrap(&store);
            }
        }
    };

    // GitButler 优先于 Teambition：每次启动先过这一关。
    // agent 靠 GitButler CLI 管分支/提交，缺它后面全白跑；git 是它的系统级依赖。
    // 例外：
    //   - gitbutler 子命令本身（就是用来装/查它的）
    //   - config/login/logout（引导前置）
    //   - ui（**关键**：窗口本身就是「显示状态 + 一键安装」的入口，
    //     拦掉它用户就没地方点安装了。窗口里会用状态卡自己检测）
    // TBA_SKIP_GITBUTLER=1 可强制跳过（排查问题用）。
    let gb_exempt = matches!(
        &cmd,
        Cmd::GitButler { .. }
            | Cmd::Config { .. }
            | Cmd::Login { .. }
            | Cmd::Logout { .. }
            // 自检是「装东西之前」就要能跑的命令：没装 but 也得能查
            // 为什么编不了，否则用户卡在一个查不了的状态里。
            // - git 通用能力（status / main / create / tag）只依赖系统 git，不依赖
            //   GitButler CLI。is_gb_project 在 but 不存在时自然返回 false 走普通 git 分支；
            //   拦住它等于「没装 GitButler 就建不了 GitHub 仓库」，纯属误伤。
            // - 项目接入链路（create / ensure-project / prompt --init）只连 Teambition，
            //   不需要 but.exe。没装 GitButler 的设备正是最需要接入的设备，拦住等于死锁。
            | Cmd::Doctor
            | Cmd::Git { .. }
            | Cmd::Create { .. }
            | Cmd::EnsureProject { .. }
            | Cmd::Prompt { init: true, .. }
            // MCP server / 配置打印：挂给陌生 agent 用的入口，缺 but 也必须能起。
            | Cmd::Serve
            | Cmd::Mcp { .. }
            // 设备接入是「什么都没装」时跑的第一条命令，缺 but.exe 更不能被拦。
            | Cmd::Onboard
            | Cmd::Ui
    );
    if !gb_exempt && std::env::var("TBA_SKIP_GITBUTLER").is_err() {
        let gb = crate::gitbutler::detect(&store);
        let git_ver = crate::gitbutler::git_available();
        if !gb.found || git_ver.is_none() {
            crate::gitbutler::print_missing_guidance(&gb, &git_ver);
            let mut missing = Vec::new();
            if !gb.found {
                missing.push("GitButler CLI");
            }
            if git_ver.is_none() {
                missing.push("git");
            }
            return Err(format!("缺少 {}（优先级高于 Teambition，先装上）", missing.join(" 和 ")));
        }
    }

    match cmd {
        Cmd::GitButler { action, path } => match action.as_str() {
            "set" => {
                let p = path.ok_or("要指定 --path <but.exe 路径，或它所在目录>")?;
                println!("{}", crate::gitbutler::set_path(&store, &p)?);
            }
            "clear" => println!("{}", crate::gitbutler::clear_path(&store)?),
            "status" => {
                let gb = crate::gitbutler::detect(&store);
                if gb.found {
                    let ver = gb.version.clone().unwrap_or_else(|| "版本未知".to_string());
                    println!(
                        "✓ GitButler CLI {ver} · 来源: {} · {}",
                        gb.source.map(|s| s.label()).unwrap_or("?"),
                        gb.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
                    );
                    match gb.project_setup {
                        Some(true) => println!("  当前目录已是 GitButler 项目"),
                        Some(false) => println!("  当前目录还没配置（跑 but setup 把它纳入 GitButler）"),
                        None => {}
                    }
                } else {
                    let git_ver = crate::gitbutler::git_available();
                    crate::gitbutler::print_missing_guidance(&gb, &git_ver);
                    return Err("未检测到 GitButler CLI".to_string());
                }
                match crate::gitbutler::git_available() {
                    Some(v) => println!("✓ git {v}"),
                    None => {
                        println!("✗ 未检测到 git: {}", crate::gitbutler::GIT_DOWNLOAD_URL);
                        return Err("未检测到 git".to_string());
                    }
                }
            }
            "install" => {
                crate::gitbutler::install(&store)?;
            }
            "path" => {
                let gb = crate::gitbutler::detect(&store);
                match gb.path {
                    Some(p) => println!("{}", p.display()),
                    None => return Err("未检测到 GitButler CLI".to_string()),
                }
            }
            other => {
                return Err(format!("未知动作: {other}（status / install / path）"));
            }
        },
        Cmd::Login { token } => {
            crate::oauth::login(&store, token)?;
        }
        Cmd::Logout => {
            crate::oauth::logout(&store)?;
        }
        Cmd::Config { show, url, tool } => {
            let mut cfg = store.load_config();
            let switched = url.is_some() || tool.is_some();
            if let Some(t) = tool {
                let t = t.trim().to_string();
                cfg.tool_path = t.clone();
                store.save_config(&cfg)?;
                if t.is_empty() {
                    println!("tool_path 已清空，文案改回用当前 exe 的真实路径。");
                } else {
                    println!("tool_path 已设为: {t}（文案里的命令都会写这个路径）");
                }
            }
            if let Some(u) = url {
                let u = u.trim().to_string();
                if u.is_empty() {
                    return Err("url 为空".to_string());
                }
                cfg.mcp_url = u;
                store.save_config(&cfg)?;
                println!("mcp_url 已切换为: {}", cfg.mcp_url);
                println!(
                    "token {}。",
                    if cfg.token().is_some() {
                        "未动，已保留"
                    } else {
                        "不存在（未登录）"
                    }
                );
            }
            if show || !switched {
                println!("mcp_url:      {}", cfg.mcp_url);
                match cfg.token() {
                    Some(t) => {
                        let masked = if t.chars().count() > 10 {
                            let head: String = t.chars().take(6).collect();
                            let tail: String = t.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
                            format!("{head}...{tail}")
                        } else {
                            "****".to_string()
                        };
                        println!("access_token: {masked} (已登录)");
                    }
                    None => println!("access_token: 未设置"),
                }
                if !cfg.tool_overrides.is_empty() {
                    println!("tool_overrides: {} 项", cfg.tool_overrides.len());
                }
                println!(
                    "tool_path:    {}",
                    if cfg.tool_path.is_empty() {
                        "(未设，用当前 exe 真实路径)"
                    } else {
                        &cfg.tool_path
                    }
                );
                println!("\n配置文件: {}", store.root.join("config.toml").display());
            }
        }
        Cmd::Projects => {
            // 离线：读上次同步下来的项目列表（缓存由在线时的 projects / 窗口自动写）。
            if store.load_config().offline() {
                let list = store.load_projects_cache();
                if list.is_empty() {
                    eprintln!(
                        "离线模式：本地还没有项目缓存。联网跑一次 `tba projects` 就会存下来。"
                    );
                }
                for (id, name) in &list {
                    println!("{id}\t{name}");
                }
                return Ok(());
            }
            let mut ctx = Ctx::new(store.clone(), false)?;
            let projects = ctx.list_projects()?;
            store.save_projects_cache(
                &projects
                    .iter()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect::<Vec<_>>(),
            )?;
            if projects.is_empty() {
                println!("没有项目。");
            }
            for p in &projects {
                println!("{}\t{}", p.id, p.name);
            }
        }
        Cmd::Init { project } => {
            // 列名统一用中文状态名：顺手把本地 config 的 tasklist_* 归一到默认值
            // （load-modify-save，token 不动）。老 config 若是数字列名，Config::load 已归一，这里落盘。
            let mut cfg = store.load_config();
            cfg.tasklist_queue = DEFAULT_TASKLIST_QUEUE.to_string();
            cfg.tasklist_claim = DEFAULT_TASKLIST_CLAIM.to_string();
            cfg.tasklist_wait = DEFAULT_TASKLIST_WAIT.to_string();
            cfg.tasklist_done = DEFAULT_TASKLIST_DONE.to_string();
            cfg.tasklist_closed = DEFAULT_TASKLIST_CLOSED.to_string();
            store.save_config(&cfg)?;

            let mut ctx = Ctx::new(store.clone(), false)?;
            let stages = ctx.ensure_stages(&project)?;
            // skill / mem 平时是懒建的（第一次用才建）。init 一次性补齐，
            // 让「7 个工作流状态」在建项目时就是齐的，看板上一眼看全。
            for aux in [SKILL_STATE, MEMORY_STATE] {
                let _ = ctx.ensure_state(&project, aux);
            }
            let stages = ctx.list_stages(&project).unwrap_or(stages);
            println!("项目 {project} 的工作流状态（7 个已自动补齐）:");
            for s in &stages {
                // skill / mem 是「用途状态」不是阶段，标出来免得看混。
                let tag = if s.name == MEMORY_STATE {
                    "  ← 用途状态：项目记忆"
                } else if s.name == SKILL_STATE {
                    "  ← 用途状态：skill"
                } else {
                    ""
                };
                println!("  {}\t{}{}", s.id, s.name, tag);
            }

            // 看板列：新版 MCP 有 task-list-create，复用同一个 ctx 自动补齐 5 个中文列，
            // 不再让人去网页手动建。补不动（老服务端 / 没分组 / 接口拒）才降级成
            // 「项目链接 + 手动补列」引导。和 create / go 共用一份，说法不漂移。
            // 查询失败不算「一列都没有」，别拿一次接口失败误导人去重建 5 列。
            let r = ctx.ensure_tasklists(&project);
            print!("{}", crate::ops::tasklist_guidance(&project, &r, true));
        }
        Cmd::EnsureProject {
            name,
            project,
            dry_run,
            yes,
        } => {
            if dry_run {
                print_name_candidates(&store, name.as_deref(), project.as_deref());
                return Ok(());
            }
            // 没给名字也没给 id：先让用户看到候选再决定。
            // 以前这里直接用目录名建——目录名经常是 code / tmp / 工具名，
            // 一建就是错名字的项目，删起来还得去网页。真要目录名就显式 --yes。
            let no_name = name.as_deref().map(str::trim).unwrap_or("").is_empty();
            let no_id = project.as_deref().map(str::trim).unwrap_or("").is_empty();
            if no_name && no_id && !yes {
                print_name_candidates(&store, None, None);
                return Err("没给项目名，先别建：让用户从上面挑一个，再跑 \
                            `tba ensure-project --name \"<选定的名字>\"`。\
                            确实要用当前目录名就加 `--yes`。"
                    .to_string());
            }
            let mut ctx = Ctx::new(store.clone(), false)?;
            // 没给名字也没给 id 时用当前目录名——这条命令本来就是「给这个目录接项目」，
            // 显式调用即代表用户同意按目录名新建。
            let want = name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(cwd_name);
            let (p, created) = ctx.ensure_project(Some(want.as_str()), project.as_deref(), true)?;
            // 补齐 5 阶段 + 记忆状态，都幂等。
            ctx.ensure_stages(&p.id)?;
            let mem = ctx.ensure_state(&p.id, MEMORY_STATE)?;
            // 记本地绑定：之后 tb prompt 不带 --project 也能认出本项目。
            let bound = write_project_binding(&p.id)?;
            // 刷新本地项目缓存：GUI 启动先用它秒开；点「刷新」还会重新拉。
            if let Ok(all) = ctx.list_projects() {
                let _ = store.save_projects_cache(
                    &all.iter()
                        .map(|x| (x.id.clone(), x.name.clone()))
                        .collect::<Vec<_>>(),
                );
            }
            println!(
                "项目就绪: {}\t{}{}",
                p.id,
                p.name,
                if created { "（新建）" } else { "（复用）" }
            );
            println!("记忆状态: {} ({})", mem.name, mem.id);
            println!("本地绑定: {}", bound.display());
            println!("下一步: tb prompt --project {}", p.id);
        }
        Cmd::Create {
            name,
            dir,
            project,
            dry,
            go,
            lang,
        } => {
            let lang = Lang::parse(&lang)
                .ok_or_else(|| format!("不认识的文案语言: {lang}（可用 zh / en）"))?;
            let target = match dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(d) => std::path::PathBuf::from(d),
                None => std::env::current_dir().map_err(|e| format!("拿不到当前目录: {e}"))?,
            };
            let target = std::fs::canonicalize(&target)
                .map_err(|e| format!("目录不存在或打不开 {}: {e}", target.display()))?;

            println!("扫描目录: {}", target.display());
            let rows = crate::create::scan(&store, &target, None);
            if rows.is_empty() {
                return Err(
                    "没找到可用的名字线索：没有 git remote，也没有 Cargo.toml / package.json 这类清单。\n\
                     直接问用户要一个项目名，再跑 `tba create --name \"<名字>\" --go`。"
                        .to_string(),
                );
            }
            println!("\n候选项目名（编号 + 名字 + 来源 + Teambition 同名情况）：");
            print_name_rows(&rows);

            if dry {
                println!("\n--dry：只列候选。没有创建项目，也没有写任何文件。");
                println!("拍板后跑：tba create --name \"<选定的名字>\" --dir \"{}\"", target.display());
                println!("要顺手写 MCP 配置并当场领任务，加 --go。");
                return Ok(());
            }

            // 给了 --name 就是用户已拍板，直接用；否则在终端里停下等选号。
            let picked = match name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(n) => n.to_string(),
                None => pick_name_interactive(&rows)?,
            };
            println!("\n已选项目名: {picked}");
            if go {
                // 自举一条龙：建项目 + 写 MCP + 当场领任务。
                // 留在 Create 分支内部调用，天然走 Create 的 GitButler 豁免，
                // 全新设备缺 but.exe 也不会被「领任务」这一环拦住。
                return finish_bootstrap(&store, &target, &picked, project.as_deref(), lang);
            }
            let r = crate::create::create(&store, &target, &picked, project.as_deref(), lang)?;
            print!("{}", r.report());
        }
        Cmd::Skills { project } => {
            // 离线：读项目能力的本地缓存（在线 list_skills 每次都会写一份）。
            let skills = if store.load_config().offline() {
                let cached = crate::ops::cached_skills(&store, &project);
                if cached.is_empty() {
                    eprintln!(
                        "离线模式：这个项目还没有能力缓存。联网打开一次（或跑 `tba skills --project {project}`）就会存下来。"
                    );
                }
                cached
            } else {
                list_skills(&store, &project)?
            };
            let switches = store.load_switches();
            let sw = switches.get(&project).cloned().unwrap_or_default();
            if skills.is_empty() {
                println!("没有 skill。用 tb skill-push 添加。");
            }
            for k in &skills {
                let on = sw.get(&k.name).cloned().unwrap_or(false);
                println!(
                    "[{}] {}\t{}",
                    if on { "开" } else { "关" },
                    k.name,
                    k.task_id
                );
            }
        }
        Cmd::Skill { name, project } => {
            // 按需读取：agent 文案里只列 skill 名字，用到哪个才来读哪个，
            // 不再把全部 skill 全文塞进文案（那是文案 token 的最大头）。
            // 离线时读缓存——文案里写的就是 `tba skill <名字>`，断网也得能拿到正文。
            let skills = if store.load_config().offline() {
                crate::ops::cached_skills(&store, &project)
            } else {
                list_skills(&store, &project)?
            };
            let k = skills
                .iter()
                .find(|k| k.name == name)
                .ok_or_else(|| {
                    format!(
                        "没有名为「{name}」的 skill。用 tb skills --project {project} 看有哪些"
                    )
                })?;
            println!("{}", k.content);
        }
        Cmd::SkillPush { project, file, name } => {
            let content = std::fs::read_to_string(&file).map_err(|e| format!("读文件失败: {e}"))?;
            let name = name.unwrap_or_else(|| {
                std::path::Path::new(&file)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "skill".to_string())
            });
            println!("{}", push_skill(&store, &project, &name, &content)?);
        }
        Cmd::SkillInit { project, name } => {
            println!(
                "{}",
                crate::ops::init_builtin_skills(&store, &project, name.as_deref())?
            );
        }
        Cmd::SkillRm { project, name } => {
            println!("{}", crate::ops::archive_skill(&store, &project, &name)?);
        }
        Cmd::Cap {
            action,
            name,
            summary,
            summary_en,
            body,
            off,
            always,
            no_always,
        } => {
            println!(
                "{}",
                cap_cmd(
                    &store,
                    action.as_deref().unwrap_or("list"),
                    name.as_deref(),
                    summary.as_deref(),
                    summary_en.as_deref(),
                    body.as_deref(),
                    off,
                    if always {
                        Some(true)
                    } else if no_always {
                        Some(false)
                    } else {
                        None
                    }
                )?
            );
        }
        Cmd::SkillTpl => {
            // 内置模板已经搬进本地能力库：那是跨项目的，agent 平时该用 tba cap。
            // 这里只做「还认得老命令」的指路，不再列第二份目录，免得两处说法打架。
            println!("内置模板已移到本地能力库（跨项目共享，窗口里能改）：");
            println!("  tba cap list     看有哪些");
            println!("  tba cap <名字>   读正文");
            println!("可选名字: shot / git-main / release / gui-notes");
            println!("确实要把某条钉到云端某个项目时才用: tba skill-init --project <id> --name <名>");
        }
        Cmd::SkillMem { project, open } => {
            // 项目名只用来写文件标题，取不到就用 id，不阻断导出。
            let mut ctx = Ctx::new(store.clone(), false)?;
            let pname = ctx
                .list_projects()
                .unwrap_or_default()
                .into_iter()
                .find(|p| p.id == project)
                .map(|p| p.name)
                .unwrap_or_else(|| project.clone());
            drop(ctx);
            let r = export_skill_mem(&store, &project, &pname)?;
            println!(
                "已导出：skill {} 个（启用 {}）、记忆 {} 条 {} 字",
                r.skill_count, r.skill_on, r.mem_count, r.mem_chars
            );
            println!("目录: {}", r.dir.display());
            println!("  {}", r.skill_file.display());
            println!("  {}", r.mem_file.display());
            if open {
                open_folder(&r.dir)?;
                println!("已打开文件夹。");
            }
        }
        Cmd::Switch { project, skill, on, off } => {
            if !on && !off {
                return Err("必须指定 --on 或 --off".to_string());
            }
            store.set_switch(&project, &skill, on)?;
            println!("skill「{skill}」已{}。", if on { "开启" } else { "关闭" });
        }
        Cmd::Tasks { project, stage } => {
            let mut ctx = Ctx::new(store.clone(), false)?;
            let stages = ctx.ensure_stages(&project)?;
            let names: Vec<String> = match &stage {
                Some(s) => vec![s.clone()],
                None => ALL_STAGES.iter().map(|s| s.to_string()).collect(),
            };
            for n in names {
                let st = ctx
                    .find_stage(&stages, &n)
                    .ok_or_else(|| format!("缺少「{n}」阶段"))?;
                let tasks = ctx.list_tasks(&project, st)?;
                println!("== {} ({}) ==", n, tasks.len());
                for t in &tasks {
                    println!("  {}\t{}\t{}", t.id, t.created, t.title);
                }
            }
        }
        Cmd::Show { target } => {
            let task_id = parse_task_ref(&target);
            if task_id.is_empty() {
                return Err("没解析出任务 id。用法: tb show https://www.teambition.com/task/<id>".into());
            }
            let mut ctx = Ctx::new(store.clone(), false)?;
            let project_id = ctx.task_project_id(&task_id)?;
            let stages = ctx.ensure_stages(&project_id)?;
            let t = ctx.task_info(&task_id)?;
            let stage_name = stages
                .iter()
                .find(|s| s.id == t.stage_id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "(未知)".to_string());
            let list_name = ctx.task_list_name(&project_id, &task_id);
            let done = stage_name == STAGE_DONE || stage_name == STAGE_CLOSED;
            println!("task_id:   {}", t.id);
            println!("task_name: {}", t.title);
            println!("project:   {project_id}");
            println!("状态:      {stage_name}");
            println!("看板列:    {}", if list_name.is_empty() { "(未知)" } else { &list_name });
            println!("是否完成:  {}", if done { "是" } else { "否" });
            if let Some(c) = store.find_active_claim(&task_id) {
                println!(
                    "本地台账:  {} / {} / 心跳 {}",
                    c.claim_id,
                    c.state,
                    c.heartbeat()
                );
            }
        }
        Cmd::Go {
            project,
            lang,
            no_entry,
            off,
            task,
        } => {
            use std::io::Write;
            let lang = Lang::parse(&lang)
                .ok_or_else(|| format!("不认识的文案语言: {lang}（可用 zh / en）"))?;
            // 不带 --off 时 go 是「去队列领任务」，不认任务描述——
            // 静默忽略会让人以为活儿已经派下去了，所以直接说清楚。
            if !off && !task.is_empty() {
                return Err(format!(
                    "`go` 不带 --off 是从队列领任务，不吃任务描述。\n\
                     要直连（不领任务、直接在当前对话里交互）请写：tba go -off {}",
                    task.join(" ")
                ));
            }
            // 项目从本地绑定 + 项目缓存解析，能力读本地缓存，
            // `-off` 后面那句话就是本次任务。
            if off {
                let desc = task.join(" ");
                // --project 给了就用它（能带出项目能力和编译参数），没给就按本地绑定。
                let pid = project
                    .clone()
                    .or_else(read_project_binding)
                    .unwrap_or_default();
                let cached = store.load_projects_cache();
                let pname = if pid.is_empty() {
                    std::env::current_dir()
                        .ok()
                        .and_then(|d| {
                            d.file_name().map(|n| n.to_string_lossy().to_string())
                        })
                        .unwrap_or_else(|| "当前目录".to_string())
                } else {
                    cached
                        .iter()
                        .find(|(id, _)| id == &pid)
                        .map(|(_, n)| n.clone())
                        .unwrap_or_else(|| "未命名项目".to_string())
                };
                let skills = if pid.is_empty() {
                    Vec::new()
                } else {
                    cached_skills(&store, &pid)
                };
                let draft = if desc.trim().is_empty() {
                    None
                } else {
                    // 复用 draft_for_mode 的标题提取，但**不落盘**：
                    // 直连模式没必要在 data/tasks/ 下留一个没人会读的正文文件。
                    draft_for_mode(&store, &desc, CopyMode::ThisTask).map(|mut d| {
                        d.body_path.clear();
                        d
                    })
                };
                print!(
                    "{}",
                    direct_prompt(&store, &pid, &pname, &skills, draft.as_ref(), lang)
                );
                eprintln!(
                    "直连模式（--off）：没领任务、没写 Teambition。有疑问直接在对话里问用户，干完直接说结论。"
                );
                let _ = std::io::stdout().flush();
                return Ok(());
            }
            claim_and_print(&store, project.as_deref(), lang, !no_entry, true)?;
        }
        Cmd::Subagent { target, project, lang } => {
            use std::io::Write;
            let lang = Lang::parse(&lang)
                .ok_or_else(|| format!("不认识的文案语言: {lang}（可用 zh / en）"))?;
            let zh = lang == Lang::Zh;
            // target 给了：派一条「已领」的任务（claim_id 或 task id）。
            // 没给：领一条「未完成」新任务（和 tba go 一样）。
            let claim = if let Some(t) = &target {
                let t = t.trim();
                let claims = store.active_claims();
                // 1) 直接是 claim_id
                if let Some(c) = claims.iter().find(|c| c.claim_id == t) {
                    c.clone()
                } else {
                    // 2) 任务链接 / 裸 task id：去掉链接前缀和查询串
                    let tid = t
                        .trim_start_matches("https://www.teambition.com/task/")
                        .split(['?', '#'])
                        .next()
                        .unwrap_or(t)
                        .to_string();
                    claims
                        .iter()
                        .find(|c| c.task_id == tid)
                        .cloned()
                        .ok_or_else(|| {
                            format!(
                                "找不到 claim_id 为「{t}」或 task_id 为「{tid}」的已领任务。先 `tba go` 或 `tba take <任务链接>` 领任务，再派子代理。"
                            )
                        })?
                }
            } else {
                // 领新任务：项目解析同 Claim（--project 优先，否则本地绑定）。
                let pid = resolve_project(project.as_deref())?;
                // 领之前先回收死会话，别让一个已经不存在的会话占着任务。
                for m in sweep(&store, false).unwrap_or_default() {
                    eprintln!("{m}");
                }
                match claim(&store, &pid, None, &current_agent()) {
                    Ok(c) => c,
                    Err(e) if e.starts_with(BLOCKED_PREFIX) => {
                        return Err(format!(
                            "手上有在等回复但没人接的任务，先处理掉再来领：{}",
                            e.trim_start_matches(BLOCKED_PREFIX)
                        ));
                    }
                    Err(e) if e.starts_with(NO_TASK_PREFIX) => {
                        return Err("队列（列「未完成」）为空，没有可领任务派给子代理。".to_string());
                    }
                    Err(e) => return Err(e),
                }
            };
            // 任务正文：派已领任务时走离线——台账里的 task_name 即标题，
            // 正文由子代理按 task_id 链接自己读（见简报末尾），避免为取正文联网。
            // 领新任务时 go() 已经联网，这里沿用同一条 claim，不二次联网。
            let skills = cached_skills(&store, &claim.project_id);
            print!("{}", subagent_brief(&store, &claim, &claim.task_name, &skills, lang));
            if zh {
                eprintln!(
                    "已生成子代理简报。主代理读这份简报、调 subagent 工具派活；干完按末尾指引收尾（不要自己 done）。"
                );
            } else {
                eprintln!("Subagent brief printed. Main agent dispatches via the subagent tool; close per the footer.");
            }
            let _ = std::io::stdout().flush();
        }
        Cmd::Claim { project, name } => {
            let project = resolve_project(project.as_deref())?;
            // 领之前先回收死会话，别让一个已经不存在的会话占着任务。
            for m in sweep(&store, false).unwrap_or_default() {
                eprintln!("{m}");
            }
            let c = match claim(&store, &project, name.as_deref(), &current_agent()) {
                Ok(c) => c,
                Err(e) if e.starts_with(BLOCKED_PREFIX) => {
                    // 跟 tba g 一致：手上有没人接的「待回复」，先处理掉再来领。
                    println!("BLOCKED");
                    eprintln!("{}", e.trim_start_matches(BLOCKED_PREFIX));
                    let _ = std::io::stdout().flush();
                    std::process::exit(5);
                }
                Err(e) => return Err(e),
            };
            println!("claim_id: {}", c.claim_id);
            println!("task_id:   {}", c.task_id);
            println!("task_name: {}", c.task_name);
            println!("状态:      {} -> {}", c.stage_from, c.stage_to);
        }
        Cmd::Take { target, agent } => {
            let agent = agent.unwrap_or_else(current_agent);
            let r = take(&store, &target, &agent)?;
            for w in &r.warnings {
                eprintln!("警告: {w}");
            }
            if r.existed {
                println!("该任务已被领取，直接复用原 claim（幂等）：");
            }
            let c = &r.claim;
            println!("claim_id: {}", c.claim_id);
            println!("task_id:   {}", c.task_id);
            println!("task_name: {}", c.task_name);
            println!("project:   {}", c.project_id);
            println!("状态:      {} -> {}", c.stage_from, c.stage_to);
            println!(
                "看板列:    {} -> {}",
                if c.tasklist_from.is_empty() {
                    "(未知)"
                } else {
                    &c.tasklist_from
                },
                r.list_now
            );
            // 把任务正文一起打出来，agent 不用再查一次。
            let mut ctx = Ctx::new(store.clone(), false)?;
            if let Ok(t) = ctx.task_info(&c.task_id) {
                if !t.content.trim().is_empty() {
                    println!("\n---- 任务正文 ----\n{}", t.content);
                }
            }
            println!("\n接单完成。做完必须交回用户：tba ask {} --done（后台常驻监听，收到回复自动唤起会话；本轮回复可正常结束）", c.claim_id);
            println!("自定义问题用: tba ask {} -q \"...\"（选项从 3 起编号，1/2 是系统保留号）  |  放弃用: tba release {}", c.claim_id, c.claim_id);
        }
        Cmd::Start {
            project,
            title,
            note,
            file,
            agent,
        } => {
            // 正文：--note 优先，否则读 --file
            let body = match (&note, &file) {
                (Some(n), _) => n.clone(),
                (None, Some(f)) => {
                    std::fs::read_to_string(f).map_err(|e| format!("读文件失败: {e}"))?
                }
                (None, None) => String::new(),
            };
            // 标题：--title > 正文首行的 `# xxx` > 文件名
            let title = title
                .or_else(|| first_heading(&body))
                .or_else(|| {
                    file.as_ref().and_then(|f| {
                        std::path::Path::new(f)
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                    })
                })
                .ok_or("需要 --title，或给一个首行是 `# 标题` 的 --file")?;
            let agent = agent.unwrap_or_else(current_agent);
            let project = resolve_project(project.as_deref())?;
            let r = start(&store, &project, &title, &body, &agent)?;
            for w in &r.warnings {
                eprintln!("警告: {w}");
            }
            if r.existed {
                println!("同名任务已开工，复用原 claim（幂等）：");
            }
            let c = &r.claim;
            println!("claim_id: {}", c.claim_id);
            println!("task_id:   {}", c.task_id);
            println!("task_name: {}", c.task_name);
            println!("project:   {}", c.project_id);
            println!("状态:      {} -> {}", c.stage_from, c.stage_to);
            println!(
                "看板列:    {} -> {}",
                if c.tasklist_from.is_empty() {
                    "(未知)"
                } else {
                    &c.tasklist_from
                },
                r.list_now
            );
            println!("\n开工完成。做完必须交回用户：tba ask {} --done（后台常驻监听，收到回复自动唤起会话）", c.claim_id);
            println!("自定义问题用: tba ask {} -q \"...\"（选项从 3 起编号，1/2 是系统保留号）  |  放弃用: tba release {}", c.claim_id, c.claim_id);
        }
        Cmd::Ask {
            claim_id,
            question,
            done,
            no_wait,
            off,
        } => {
            let q = if done {
                DONE_QUESTION.to_string()
            } else {
                question.ok_or(
                    "要给问题：`-q \"...\"`（自定义选项从 3 起编号，1/2 是系统保留号），或者加 `--done` 用标准的「做完了请确认」三选项",
                )?
            };
            // 直连模式：不写任务、不发信、不阻塞。问题直接打到 stdout，
            // 由 agent 在对话框里问——用户就在这儿回，不用跑去 Teambition。
            if off {
                println!("{q}");
                eprintln!(
                    "直连模式（--off）：问题没写进 Teambition、没发邮件、不阻塞。\
                     直接把上面的问题问用户，拿到回答继续。"
                );
                let _ = std::io::stdout().flush();
                return Ok(());
            }
            let claim_id = claim_id
                .ok_or("要给 claim id：`tba ask <claim_id> -q \"...\"`（直连模式可写 --off 省掉它）")?;
            let c = ask(&store, &claim_id, &q)?;
            let cfg = store.load_config();
            println!(
                "已提问。任务「{}」现在是「{}」，看板列「{}」，球在用户那边。",
                c.task_name, STAGE_WAIT, cfg.tasklist_wait
            );
            println!("问题已写进任务备注，邮件已发。");
            if no_wait {
                println!(
                    "\n（--no-wait）不等回复。想等就自己跑: tba wait {}",
                    c.claim_id
                );
            } else {
                // 常驻监听从这次 ask 起一直跑到用户回 1 收尾，执行中也不中断。
                // ask 自己只是「前台等待者」：读到一条新回复就打印并退出，唤醒会话。
                ensure_listener(&store, &c.claim_id)?;
                println!(
                    "\n常驻监听已挂上（每 {} 秒查一次评论/邮件，直到你回 1 才停；执行中也不中断）。\n\
                     邮件和 Teambition 上的每条新回复都会唤醒会话；别重跑 ask（会重复发邮件）。",
                    cfg.wait_interval_secs
                );
                wait_for_feed(&store, &c.claim_id)?;
            }
        }
        Cmd::Reply { claim_id } => {
            let (c, replies) = resume(&store, &claim_id)?;
            println!(
                "任务「{}」已改回「{}」，看板列「{}」。",
                c.task_name,
                STAGE_DOING,
                store.load_config().tasklist_claim
            );
            if replies.is_empty() {
                println!("\n（还没读到你的新评论。可能你还没回复，或回复时间早于提问时间。）");
            } else {
                println!("\n---- 你的回复 ----");
                for r in &replies {
                    println!("[{}] {}\n{}", r.created, r.author, r.text);
                }
            }
        }
        Cmd::Hello { name } => {
            let (tag, path) = set_session(name.as_deref())?;
            println!("会话标识: {tag}");
            println!("已写入:   {}", path.to_string_lossy());
            println!(
                "\n之后本目录下所有 tb 命令自动用这个标识（也可以用 TBA_AGENT 环境变量覆盖）。\n\
                 同一目录要跑多个会话，用 tba hello --name wb-a / --name wb-b 分别指定，\n\
                 否则两个会话会共用一个标识，会话存活判定会互相顶。"
            );
        }
        Cmd::Inbox { agent } => {
            let list = inbox(&store, agent.as_deref());
            if list.is_empty() {
                println!("没有任务在等你回复。");
            } else {
                println!("{} 条任务在等你回复（等得最久的在前）:\n", list.len());
                for c in &list {
                    let ago = human_age(crate::store::age_secs(&c.ask_at));
                    // 等待心跳停了 = agent 没在等（丢后台 / 进程没了）。
                    let broken = match store.wait_age(&c.claim_id) {
                        Some(a) => a > ORPHAN_WAIT_SECS,
                        None => crate::store::age_secs(&c.ask_at) > ORPHAN_WAIT_SECS * 2,
                    };
                    println!(
                        "• {}  [{}]{}",
                        c.task_name,
                        c.claim_id,
                        if broken { "  ⚠ 等待中断（没人在等）" } else { "" }
                    );
                    println!("  等了 {ago}  ·  会话 {}", c.agent);
                    if !c.question.trim().is_empty() {
                        println!("  问题: {}", c.question.lines().next().unwrap_or(""));
                    }
                    println!("  {}", task_url(&c.task_id));
                    println!();
                }
                println!(
                    "在 Teambition 或邮件里回复后：\n  \
                     那个会话如果还卡在 `tba ask`，会自动收到回复继续；\n  \
                     已经断了就手动 `tba reply <claim_id>` 看一眼。"
                );
            }
        }
        Cmd::Wait {
            claim_id,
            timeout,
            interval,
            once,
            off,
            listen,
        } => {
            // 直连模式没有「等 Teambition 回复」这回事，直接返回别让人干等。
            if off {
                eprintln!(
                    "直连模式（--off）：不轮询 Teambition。要等用户回答就直接在对话里问。"
                );
                return Ok(());
            }
            let claim_id = claim_id.ok_or("要给 claim id：`tba wait <claim_id>`")?;
            if listen {
                // detached 监听本体：只有用户回 1 / 别处收尾才退出。
                run_wait_block(&store, &claim_id, timeout, interval, true, true)?;
            } else if once {
                // --once 老路径：单轮到点退出码 2，收到一条回复就返回。
                run_wait_block(&store, &claim_id, timeout, interval, false, false)?;
            } else {
                // agent 续等：确保常驻监听在跑，读到下一条新回复就退出唤醒会话。
                ensure_listener(&store, &claim_id)?;
                wait_for_feed(&store, &claim_id)?;
            }
        }
        Cmd::Events { claim, limit } => {
            let rows = crate::events::recent(
                &store.root,
                limit.unwrap_or(20),
                claim.as_deref(),
            );
            if rows.is_empty() {
                println!("还没有记录到执行中收到的回复。");
            } else {
                for e in &rows {
                    println!("[{}] {} 来源={}\n  {}", &e.ts, &e.claim_id, &e.source, &e.text);
                }
                println!("\n共 {} 条。`tba events` 只列出，不改任务状态。", rows.len());
            }
        }
        Cmd::Beat { claim_id } => {
            let c = beat(&store, &claim_id)?;
            println!("已续命: {} ({})", c.claim_id, c.state);
        }
        Cmd::Note {
            claim_id,
            text,
            file,
        } => {
            // 内容优先取 --file（长会话记录通常是文件），否则 --text。
            let body = match (&file, &text) {
                (Some(f), _) => {
                    std::fs::read_to_string(f).map_err(|e| format!("读文件失败 {f}: {e}"))?
                }
                (None, Some(t)) => t.clone(),
                (None, None) => {
                    return Err("要给内容：--text \"...\" 或 --file 路径".to_string())
                }
            };
            println!("{}", append_note(&store, &claim_id, &body)?);
        }
        Cmd::Mail { action, arg } => {
            let cfg = store.load_config();
            match action.as_str() {
                "status" => {
                    println!("SMTP: {}:{} 用户={} 发件人={} 收件人={}",
                        if cfg.smtp_host.is_empty() { "(未配)" } else { &cfg.smtp_host },
                        cfg.smtp_port,
                        if cfg.smtp_user.is_empty() { "(未配)" } else { &cfg.smtp_user },
                        if cfg.smtp_from.is_empty() { "(用账号)" } else { &cfg.smtp_from },
                        if cfg.mail_to.is_empty() { "(未配)" } else { &cfg.mail_to });
                    println!("IMAP: {}:{}", if cfg.imap_host.is_empty() { "(未配，收不到回信)" } else { &cfg.imap_host }, cfg.imap_port);
                    println!("到「待回复」自动发信: {}", if cfg.mail_on_ask { "开" } else { "关" });
                    println!("能发信: {} / 能收信: {}", mail::can_send(&cfg), mail::can_receive(&cfg));
                    println!(
                        "等待: --once 单轮最长 {} 秒 · 评论轮询 {} 秒 · 邮件轮询 {} 秒（常驻模式不超时；GUI 设置页可改）",
                        cfg.wait_timeout_secs, cfg.wait_interval_secs, cfg.mail_poll_secs
                    );
                }
                "test" => {
                    mail::send(&cfg, "[TB] 测试邮件", "这是一封 tba 测试邮件。收到即表示 SMTP 配置可用。")?;
                    println!("已发送测试邮件到 {}", cfg.mail_to);
                }
                "check" => {
                    let msgs = mail::fetch_replies(&cfg, "[TB")?;
                    if !msgs.is_empty() {
                        for m in &msgs {
                            println!("---- 回信 ----\n{m}\n");
                        }
                        println!("共 {} 封回信。", msgs.len());
                    } else {
                        println!("没有匹配到回信（未读 + 主题/正文带 [TB…] 标记）。");
                        // 光说「没有」没法定位，顺手把最近几封列出来：
                        // 能看到「信到了但标记不对」和「信根本没到」的区别。
                        match mail::recent(&cfg, 5) {
                            Ok(rows) if !rows.is_empty() => {
                                println!("\nINBOX 最近 {} 封（核对是不是标记对不上）:", rows.len());
                                for r in rows {
                                    println!("  #{} {}", r.uid, r.subject);
                                    if !r.snippet.is_empty() {
                                        println!("       {}", r.snippet);
                                    }
                                }
                            }
                            Ok(_) => println!("INBOX 是空的——确认客户回的是 {} 这个信箱。", cfg.smtp_user),
                            Err(e) => println!("（顺带读最近邮件也失败了: {e}）"),
                        }
                    }
                }
                "resend" => {
                    let cid = arg.ok_or(
                        "要给 claim_id：`tba mail resend <claim_id>`（用 tba inbox 查）",
                    )?;
                    let to = resend_mail(&store, &cid)?;
                    println!("已重发通知邮件到 {to}（正文带完整任务备注）");
                }
                other => {
                    return Err(format!("不认识的邮件动作: {other}（可用 status / test / check / resend）"))
                }
            }
        }
        Cmd::Raw { tool, args, list } => {
            // 调试口：直接调任意 Teambition MCP 工具，打印原始 JSON。
            // 存在的意义是「查接口不用改代码」——Teambition 的字段语义经常跟
            // 名字对不上（note 落到标题就是例子），实测比读文档快。
            let mut ctx = Ctx::new(store.clone(), false)?;
            if list || tool.is_empty() {
                let mut names: Vec<&str> = ctx.tb.tools.iter().map(|t| t.name.as_str()).collect();
                names.sort();
                println!("共 {} 个工具:", names.len());
                for n in names {
                    println!("  {n}");
                }
                return Ok(());
            }
            let a: serde_json::Value = match args {
                Some(s) => serde_json::from_str(&s)
                    .map_err(|e| format!("--args 不是合法 JSON: {e}"))?,
                None => serde_json::json!({}),
            };
            let res = ctx.tb.call(&tool, a)?;
            println!("{}", serde_json::to_string_pretty(&res).unwrap_or_default());
        }
        Cmd::Release { claim_id, reason } => {
            let c = release(&store, &claim_id, reason.as_deref().unwrap_or(""))?;
            // 列移不动：Teambition 的 MCP 没有移列工具（move_to_list 映射为空），
            // 所以只能说状态改回了，列得用户自己拖。以前这里写「+ 列1」，是骗人的。
            println!(
                "任务「{}」已退回队列：状态改回「{}」，看板列请手动拖到「{}」。",
                c.task_name,
                STAGE_TODO,
                store.load_config().tasklist_queue
            );
        }
        Cmd::Sweep { force } => {
            let done = sweep(&store, force)?;
            if done.is_empty() {
                println!("没有需要回收的会话。");
            }
            for m in done {
                println!("{m}");
            }
        }
        Cmd::Done { claim_id } => {
            let c = finish(&store, &claim_id, STAGE_DONE)?;
            println!("任务「{}」已转为「已解决」。claim_id: {}", c.task_name, c.claim_id);
        }
        Cmd::Close { claim_id } => {
            let c = finish(&store, &claim_id, STAGE_CLOSED)?;
            println!("任务「{}」已转为「{STAGE_CLOSED}」。claim_id: {}", c.task_name, c.claim_id);
        }
        Cmd::Status => {
            // 先回收死会话，让台账反映真实情况。
            for m in sweep(&store, false).unwrap_or_default() {
                println!("{m}");
            }
            let claims = store.load_claims();
            if claims.is_empty() {
                println!("没有领取记录。");
                return Ok(());
            }
            let active: Vec<_> = claims.iter().filter(|c| c.is_active()).collect();
            let history: Vec<_> = claims.iter().filter(|c| !c.is_active()).collect();
            let label = |s: &str| -> String {
                match s {
                    "running" | "claimed" => "执行中".to_string(),
                    "waiting" => "等你回复".to_string(),
                    "done" => "已解决".to_string(),
                    "closed" => STAGE_CLOSED.to_string(),
                    "reverted" => "已回收".to_string(),
                    "released" => "已退回".to_string(),
                    "failed" => "失败".to_string(),
                    other => other.to_string(),
                }
            };
            if active.is_empty() {
                println!("当前没有进行中的任务。");
            } else {
                println!("== 进行中 ==");
                for c in &active {
                    let age = crate::store::age_secs(c.heartbeat());
                    println!(
                        "{}\t{}\t{}\t{}\t心跳 {} 秒前",
                        c.claim_id,
                        label(&c.state),
                        c.task_name,
                        c.project_id,
                        age
                    );
                    if c.state == "waiting" && !c.question.is_empty() {
                        println!("    等回复: {}", c.question);
                    }
                }
            }
            if !history.is_empty() {
                println!("\n== 历史 ==");
                for c in history.iter().rev().take(20) {
                    println!(
                        "{}\t{}\t{}\t{}",
                        c.claim_id,
                        label(&c.state),
                        c.task_name,
                        c.created_at
                    );
                }
            }
        }
        Cmd::Prompt {
            project,
            name,
            task,
            task_file,
            lang,
            init,
        } => {
            let lang = Lang::parse(&lang)
                .ok_or_else(|| format!("不认识的文案语言: {lang}（可用 zh / en）"))?;
            // 项目接入文案：只输出指令，不解析、不新建项目——建不建由 agent 自己判断。
            // 不给 --name 就让 agent 用仓库目录名当项目名。
            if init {
                print!("{}", crate::ops::init_prompt(&store, name.as_deref(), lang));
                return Ok(());
            }
            let mut ctx = Ctx::new(store.clone(), false)?;
            // 项目解析：--project 优先；--name 给了就允许新建；都没给则靠 .tba/project 绑定。
            let (p, created) =
                ctx.ensure_project(name.as_deref(), project.as_deref(), name.is_some())?;
            if created {
                eprintln!(
                    "已在 Teambition 新建项目「{}」（{}）",
                    p.name, p.id
                );
            }
            if created || name.is_some() {
                let _ = write_project_binding(&p.id);
            }
            if created {
                if let Ok(all) = ctx.list_projects() {
                    let _ = store.save_projects_cache(
                        &all.iter()
                            .map(|x| (x.id.clone(), x.name.clone()))
                            .collect::<Vec<_>>(),
                    );
                }
            }
            let project = p.id;
            let name = p.name;

            // 组装「本次任务」。标题优先 --task 的首行，正文优先 --task-file。
            let mut title = String::new();
            let mut body = String::new();
            if let Some(t) = &task {
                if let Some(h) = first_heading(t) {
                    title = h;
                }
                body = t.clone();
            }
            if let Some(f) = &task_file {
                let s = std::fs::read_to_string(f).map_err(|e| format!("读文件失败: {e}"))?;
                if title.is_empty() {
                    title = first_heading(&s).unwrap_or_else(|| {
                        std::path::Path::new(f)
                            .file_stem()
                            .map(|x| x.to_string_lossy().to_string())
                            .unwrap_or_else(|| "本次任务".to_string())
                    });
                }
                body = s;
            }
            let draft = if body.trim().is_empty() && task.is_none() && task_file.is_none() {
                None
            } else {
                if title.is_empty() {
                    return Err(
                        "--task / --task-file 里没找到任务标题。给首行是 `# 标题` 的正文即可"
                            .to_string(),
                    );
                }
                // 正文落盘：文案里只给路径，agent 用 --file 引用，不用手抄长正文。
                let path = save_task_draft(&store, &title, &body)?;
                Some(TaskDraft {
                    title,
                    body,
                    body_path: path,
                })
            };
            print!(
                "{}",
                agent_prompt(&store, &project, &name, draft.as_ref(), lang)?
            );
        }
        Cmd::Memory {
            project,
            append,
            file,
            section,
            overwrite,
            force,
            on,
            off,
            status,
            local,
            import_local,
            init,
        } => {
            let mut m = store.load_tb_memory(&project);
            // 建记忆状态 + 检查。幂等，可反复跑。
            if init {
                println!("记忆状态: {}", ensure_memory_state(&store, &project)?);
                println!();
                print!("{}", project_memory_status(&store, &project)?);
                return Ok(());
            }
            // 开关。关掉不删任何东西——记忆任务还在，只是 tb 不读了。
            if on {
                m.enabled = true;
                store.save_tb_memory(&project, &m)?;
                println!("已启用本项目的 tb 共享记忆。");
                println!(
                    "记忆 = 状态为「{}」的普通任务：一条记忆一条任务，正文在备注里。",
                    m.state()
                );
                println!(
                    "⚠️ 建议同时关闭各 agent 自带的记忆（如 WorkBuddy 的 .workbuddy-ai/memory/MEMORY.md），\n\
                     否则两份记忆会打架。tb 不会代你改它们的文件。"
                );
                println!("开工文案里会自动加一句「开工先读项目记忆」。");
                return Ok(());
            }
            if off {
                m.enabled = false;
                store.save_tb_memory(&project, &m)?;
                println!("已关闭本项目的 tb 共享记忆。");
                println!(
                    "记忆任务仍在（状态为「{}」），没有删除。重新打开用 tba memory --on。",
                    m.state()
                );
                return Ok(());
            }
            if status {
                print!("{}", project_memory_status(&store, &project)?);
                return Ok(());
            }
            if local {
                let files = store.read_memory(&project);
                if files.is_empty() {
                    println!(
                        "本地记忆目录为空。目录: {}",
                        store.memory_dir(&project).display()
                    );
                }
                for (name, content) in files {
                    println!("===== {name} =====");
                    println!("{content}");
                }
                return Ok(());
            }
            if import_local {
                println!("{}", import_local_memory(&store, &project)?);
                return Ok(());
            }
            // 写：--overwrite 覆盖整节；--append / --file 追加一条。
            if let Some(text) = overwrite {
                println!(
                    "{}",
                    overwrite_memory(&store, &project, &section, &text, force)?
                );
                return Ok(());
            }
            // 内容优先取 --file（长文本走文件），否则 --append。
            let body = match (&file, &append) {
                (Some(f), _) => {
                    std::fs::read_to_string(f).map_err(|e| format!("读文件失败 {f}: {e}"))?
                }
                (None, Some(t)) => t.clone(),
                (None, None) => String::new(),
            };
            if body.trim().is_empty() {
                // 都没给 = 读。开关关着会报错（不静默返回空）。
                let pm = read_project_memory(&store, &project)?;
                if pm.text.trim().is_empty() {
                    println!("（记忆任务都是空的。条目数: {}）", pm.entries.len());
                } else {
                    println!("{}", pm.text.trim_end());
                }
                return Ok(());
            }
            println!(
                "{}",
                append_memory(&store, &project, &section, &body, force)?
            );
        }
        Cmd::Tools {
            refresh,
            json,
            overrides,
        } => {
            let ctx = Ctx::new(store.clone(), refresh)?;
            if overrides {
                print!("{}", ctx.map.overrides_toml());
                return Ok(());
            }
            if json {
                // 完整 schema，用来排查参数问题
                println!("[");
                for (i, t) in ctx.tb.tools.iter().enumerate() {
                    let comma = if i + 1 == ctx.tb.tools.len() { "" } else { "," };
                    println!(
                        "  {{\"name\":{},\"description\":{},\"inputSchema\":{}}}{}",
                        serde_json::to_string(&t.name).unwrap_or_default(),
                        serde_json::to_string(&t.description).unwrap_or_default(),
                        serde_json::to_string(&t.input_schema).unwrap_or_default(),
                        comma
                    );
                }
                println!("]");
                return Ok(());
            }
            println!("MCP 工具 ({} 个):", ctx.tb.tools.len());
            for t in &ctx.tb.tools {
                println!("  {}\t{}", t.name, t.description);
            }
            println!("\n角色识别结果:");
            for (role, tool) in ctx.map.summary() {
                println!("  {role}\t{tool}");
            }
            // 手写的覆盖值过期时不能静默：这类问题表现为「某个命令单独坏掉」，
            // 报错只说找不到角色，很难联想到 config 里那行旧名字。
            if !ctx.map.stale_overrides.is_empty() {
                println!("\n⚠ config.toml [tool_overrides] 里这些名字在当前工具集里不存在，已忽略：");
                for (role, tool) in &ctx.map.stale_overrides {
                    println!("  {role} = \"{tool}\"");
                }
                println!("  跑 `tba tools --overrides` 拿一份可用的片段覆盖掉它们。");
            }
        }
        Cmd::Serve => {
            crate::mcp_server::serve(&store)?;
        }
        Cmd::Mcp { install } => {
            if install {
                println!("{}", mcp_install()?);
            } else {
                println!(
                    "把下面这段加进 agent 的 MCP 配置（每台设备配一次）：\n\n{}\n\n\
                     本机一键写入 WorkBuddy：`tba mcp --install`\n\
                     工具列表由 MCP 协议自动下发，不用手记名字。",
                    mcp_snippet()
                );
            }
        }
        Cmd::Onboard => {
            onboard(&store)?;
        }
        Cmd::Ui => {
            crate::ui::run(&store)?;
        }
        Cmd::Shot {
            out,
            title,
            hwnd,
            level,
        } => {
            println!(
                "{}",
                crate::shot::capture(&out, title.as_deref(), hwnd, &level)?
            );
        }
        Cmd::Doctor => {
            println!("{}", doctor(&store));
        }
        Cmd::Git {
            action,
            message,
            branch,
            land,
            tag,
            name,
            private,
            remote,
            dry,
        } => match action.as_str() {
            "status" | "st" => println!("{}", crate::git::status()?),
            "main" | "push" | "update" => println!(
                "{}",
                crate::git::main_push(message.as_deref(), branch.as_deref(), land, dry)?
            ),
            "create" | "new" => println!(
                "{}",
                crate::git::create_repo(name.as_deref(), private, &remote, dry)?
            ),
            "tag" => {
                let t = tag
                    .as_deref()
                    .ok_or("git tag 需要 --tag v1.2.3（或用 tba release）")?;
                println!("{}", crate::git::tag_push(t, message.as_deref(), dry, false)?);
            }
            other => {
                return Err(format!(
                    "未知动作: {other}（可选 status | main | update | create | tag）"
                ))
            }
        },
        Cmd::Publish {
            tag,
            watch,
            repo,
            no_push,
            init,
            force,
            timeout,
        } => {
            if init {
                println!("{}", crate::git::init_workflow(None, force)?);
            } else {
                println!(
                    "{}",
                    crate::git::release(
                        tag.as_deref(),
                        watch,
                        repo.as_deref(),
                        no_push,
                        timeout
                    )?
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{mcp_command, mcp_snippet};

    /// Windows 路径含反斜杠，片段必须是能被客户端直接解析的合法 JSON。
    #[test]
    fn mcp_snippet_is_valid_json_with_abs_command() {
        let v: serde_json::Value =
            serde_json::from_str(&mcp_snippet()).expect("tba mcp 片段必须是合法 JSON");
        let tba = &v["mcpServers"]["tba"];
        assert_eq!(tba["args"][0], "serve");
        assert_eq!(tba["command"], mcp_command());
        assert_ne!(tba["command"], "tba.exe", "别把裸命令写进宿主配置");
        assert!(
            tba["command"]
                .as_str()
                .unwrap()
                .contains(std::path::MAIN_SEPARATOR),
            "command 应该是 exe 绝对路径"
        );
    }
}
