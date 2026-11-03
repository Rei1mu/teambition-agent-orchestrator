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
    // 构建工具链：编译 & 重载要靠 link.exe + SDK 的库。缺了 agent 会以为是代码问题，
    // 其实是这台机器没配路径——这一条能直接定位，省掉一轮瞎试。
    let tcr = crate::toolchain::resolve(store);
    s.push_str(&format!(
        "编译:  {}\n",
        if tcr.ready() {
            format!("MSVC 可用（{}）", tcr.source.label())
        } else {
            format!(
                "不可用（{}）：{} —— 用 tba toolchain set 指定位置",
                tcr.source.label(),
                tcr.problems.join("；")
            )
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

fn fail_msg(action: &str, r: &crate::capability::Outcome) -> String {
    if r.err.is_empty() {
        format!("reload {action} 失败 (exit {})", r.code)
    } else {
        r.err.clone()
    }
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
            "没指定项目，当前目录也没绑定。先 `tba ensure-project`（项目名取目录名），\
             或加 --project <id>"
                .to_string(),
        ),
    }
}

// 阻塞等用户回数字，直到回复 / 超时 / 别处收尾。
//
// 抽成独立函数是因为 ask 和 wait 都要用它：ask 问完就地开始等，
// agent 只需要记一条命令，没有「问完忘了等」的中间态。
//
// 退出码约定（agent 靠这个决定下一步，不要改）：
//   正常返回 = 收到回复，正文和 action 已打印
//   2        = 单轮超时（默认 540 秒），agent 再跑一次即可，别结束会话
//   3        = 用户在网页上手动收尾了，停
fn run_wait_block(
    store: &Store,
    claim_id: &str,
    timeout: Option<u64>,
    interval: Option<u64>,
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
    match wait(store, claim_id, t, iv)? {
        WaitOutcome::Replied {
            claim: c,
            replies,
            action,
            body,
            waited_secs,
        } => {
            if action == ReplyAction::Closed {
                println!(
                    "收到回复 0（等了 {waited_secs} 秒）。任务「{}」已收尾：「{}」+ 看板列「{}」。",
                    c.task_name, STAGE_DONE, cfg.tasklist_done
                );
            } else {
                println!(
                    "收到回复（等了 {waited_secs} 秒）。任务「{}」已回到「{}」+ 看板列「{}」。",
                    c.task_name, STAGE_DOING, cfg.tasklist_claim
                );
            }
            // 结构化标记：agent 看 action 决定下一步，不用自己解析中文。
            let num = action.num().map(|n| n.to_string()).unwrap_or_else(|| "-".into());
            println!("reply={num} action={} text={:?}", action.tag(), body);
            println!("\n---- 你的回复 ----");
            for r in &replies {
                println!("[{}] {}\n{}", r.created, r.author, r.text);
            }
            println!();
            match &action {
                ReplyAction::Closed => {
                    println!("已收尾，停。别再跑 done/close。");
                }
                ReplyAction::Summarize => {
                    println!(
                        "用户要求先把这次的经验写进项目记忆：\n  tba memory --append --section 踩坑 \"...\"\n写完再进待回复：\n  tba ask {} --done",
                        c.claim_id
                    );
                }
                ReplyAction::Recheck => {
                    println!(
                        "用户说没解决。自查后重新给选项：\n  tba ask {} -q \"...\\n\\n回复数字：\\n0 = 已解决，收尾\\n1 = 已解决，先写进项目记忆\\n2 = 还没好，我再查\\n3 = <你自己的选项>\"",
                        c.claim_id
                    );
                }
                ReplyAction::Custom(n) => {
                    println!("用户选了你自定义的选项 {n}，按你在问题里写的语义执行。干完汇报。");
                }
                ReplyAction::Free => {
                    println!("用户没回数字，按原文理解。干完汇报。");
                }
            }
            Ok(())
        }
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
                 （退出码 2 = 本轮超时，不是失败。别结束会话，重跑就行。）",
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


// 打印设置时把空值显式标出来：空字符串在终端里看不见，容易以为命令没生效。
fn dash(s: &str) -> &str {
    if s.trim().is_empty() {
        "（空）"
    } else {
        s
    }
}

pub fn run() -> Result<(), String> {
    let store = Store::new();
    store.ensure_dirs()?;
    let cli = crate::Cli::parse_from(normalize_args(std::env::args_os().collect()));

    // 任何命令启动都顺带给「本会话占着的 running 任务」刷一次心跳：
    // 会话还活着（还在跑 tb）就不该被当成僵尸回收，长构建期间尤其需要。
    // sweep / hello 除外——sweep 是来回收的，先续命就回收不掉了。
    if !matches!(&cli.cmd, Some(Cmd::Sweep { .. }) | Some(Cmd::Hello { .. })) {
        store.beat_agent(&current_agent());
    }
    // 不带子命令（双击 tb / 直接 tb）→ 打开 GUI 窗口。agent 调用一律走显式子命令(CLI)。
    let cmd = cli.cmd.unwrap_or(Cmd::Ui);

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
            // 工具链和自检是「装东西之前」就要能跑的命令：没装 but 也得能查
            // 为什么编不了，否则用户卡在一个查不了的状态里。
            | Cmd::Toolchain { .. }
            | Cmd::Doctor
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
        Cmd::Toolchain {
            action,
            msvc_bin,
            msvc_lib,
            sdk_lib,
            extra_lib,
        } => {
            use crate::toolchain as tc;
            match action.as_str() {
                "set" => {
                    if msvc_bin.is_none()
                        && msvc_lib.is_none()
                        && sdk_lib.is_none()
                        && extra_lib.is_none()
                    {
                        return Err(
                            "至少要给一个：--msvc-bin / --msvc-lib / --sdk-lib / --extra-lib"
                                .to_string(),
                        );
                    }
                    println!(
                        "{}",
                        tc::set(
                            &store,
                            msvc_bin.as_deref(),
                            msvc_lib.as_deref(),
                            sdk_lib.as_deref(),
                            extra_lib.as_deref()
                        )?
                    );
                }
                "guess" => println!("{}", tc::guess(&store)?),
                "clear" => println!("{}", tc::clear(&store)?),
                "path" => println!("{}", tc::path(&store).display()),
                _ => {
                    let r = tc::resolve(&store);
                    println!(
                        "构建工具链（MSVC）：{}",
                        if r.ready() { "可用" } else { "不可用" }
                    );
                    println!("  来源: {}", r.source.label());
                    println!("{}", tc::show(&r.tc));
                    if r.ready() {
                        println!("\n✓ link.exe / libcmt.lib / kernel32.lib 都在");
                    } else {
                        println!("\n✗ 还差：");
                        for p in &r.problems {
                            println!("  - {p}");
                        }
                        if r.tc.sdk_lib.trim().is_empty() {
                            println!("\nWindows SDK 的 lib 找过这些位置：");
                            println!("{}", tc::sdk_hints());
                            println!("本机没装 SDK 的话装一个（免费）：");
                            println!("  https://developer.microsoft.com/windows/downloads/windows-sdk/");
                            println!("或者在 VS Installer 里勾「Windows 11 SDK」。");
                        }
                        println!("\n指定位置（换成你机器上的实际目录）：");
                        println!("  tba toolchain set --msvc-bin <dir> --msvc-lib <dir> --sdk-lib <dir>");
                        println!("    --msvc-bin: <VS>/VC/Tools/MSVC/<ver>/bin/Hostx64/x64");
                        println!("    --msvc-lib: <VS>/VC/Tools/MSVC/<ver>/lib/x64");
                        println!("    --sdk-lib : <Windows Kits>/10/Lib/<ver>/um/x64");
                        println!("装的是标准位置就先用 `tba toolchain guess` 自动填一次。");
                        return Err("构建工具链不可用".to_string());
                    }
                }
            }
        }
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
            // 列名统一数字 1..5：顺手把本地 config 的 tasklist_* 归一到默认值
            // （load-modify-save，token 不动）。老 config 若是中文列名，这里纠偏。
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

            // 看板列：网页 API 不能建/改名列，只能手动建。校验 5 个数字列 1..5 齐全，
            // 不齐就引导用户去 Teambition 网页手动补。
            let lists = ctx.task_lists(&project).unwrap_or_default();
            let names: Vec<&str> = lists.iter().map(|l| l.name.as_str()).collect();
            let ok = tasklists_ready(&names);
            println!(
                "\n看板列（任务列表，网页维护）: 找到 {} 个 -> {}",
                lists.len(),
                if names.is_empty() { "(无)".to_string() } else { names.join(", ") }
            );
            if ok {
                println!("  ✓ 5 个数字列 1/2/3/4/5 齐全，本地列配置已设为 1/2/3/4/5。");
            } else {
                println!("  ✗ 数字列不齐。Teambition 网页 API 建不了列，请手动去项目看板补足，");
                println!("    名字就用数字、按顺序建（别改中文，太麻烦）:");
                println!("      列 1 = {STAGE_TODO}  （队列：没人接的任务）");
                println!("      列 2 = 进行中  （agent 正在干）");
                println!("      列 3 = 待回复  （等用户在 Teambition 评论回复）");
                println!("      列 4 = 已解决  （已完成）");
                println!("      列 5 = {STAGE_CLOSED}  （作废）");
                println!("    建好后重跑一次 tb init 校验。");
            }
        }
        Cmd::EnsureProject { name, project } => {
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
            println!("可选名字: shot / git-main / release / gui-notes / reload");
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
            match go(&store, project.as_deref(), lang, !no_entry)? {
                GoOutcome::NoBinding { cwd } => {
                    println!("NO_BINDING {cwd}");
                    eprintln!("当前目录没有绑定 Teambition 项目（缺 .tba/project）。停，不领任务。");
                    eprintln!(
                        "要接入：`tba ensure-project`（项目名取目录名），或 `tba ensure-project --name <名字>`"
                    );
                    let _ = std::io::stdout().flush();
                    std::process::exit(3);
                }
                GoOutcome::NoProject { id, name } => {
                    if id.is_empty() {
                        println!("NO_PROJECT {name}");
                        eprintln!(
                            "当前目录按名字「{name}」在 Teambition 上没找到项目（或撞了多个同名）。"
                        );
                    } else {
                        println!("NO_PROJECT {id}");
                        eprintln!("本地绑定指向的项目「{id}」在 Teambition 里查不到（可能被删了）。");
                    }
                    eprintln!("要初始化：`tba ensure-project --name {name}`（有同名就复用，没有就新建）");
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
            println!("\n接单完成。做完必须交回用户：tba ask {} --done（会就地阻塞等用户回数字，别结束会话）", c.claim_id);
            println!("自定义问题用: tba ask {} -q \"...\"  |  放弃用: tba release {}", c.claim_id, c.claim_id);
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
            println!("\n开工完成。做完必须交回用户：tba ask {} --done（会就地阻塞等用户回数字，别结束会话）", c.claim_id);
            println!("自定义问题用: tba ask {} -q \"...\"  |  放弃用: tba release {}", c.claim_id, c.claim_id);
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
                    "要给问题：`-q \"...\"`，或者加 `--done` 用标准的「做完了请确认」三选项",
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
                // 默认就地等：问完立刻阻塞，agent 没有「问完忘了等」的中间态。
                // 这是「会话不许断」的关键——只要 agent 跑了 ask，它就卡在这里。
                println!(
                    "\n开始等回复（单轮最长 {} 秒，每 {} 秒查一次）。用户回数字后这条命令返回，会话继续。\n\
                     超时退出码 2 = 本轮没等到，改用 `tba wait {}` 接着等（别重跑 ask，会重复发邮件），别结束会话。",
                    cfg.wait_timeout_secs, cfg.wait_interval_secs, c.claim_id
                );
                run_wait_block(&store, &c.claim_id, None, None)?;
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
            off,
        } => {
            // 直连模式没有「等 Teambition 回复」这回事，直接返回别让人干等。
            if off {
                eprintln!(
                    "直连模式（--off）：不轮询 Teambition。要等用户回答就直接在对话里问。"
                );
                return Ok(());
            }
            let claim_id = claim_id.ok_or("要给 claim id：`tba wait <claim_id>`")?;
            run_wait_block(&store, &claim_id, timeout, interval)?;
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
                        "等待: 单轮最长 {} 秒 · 评论轮询 {} 秒 · 邮件轮询 {} 秒（GUI 设置页可改）",
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
        Cmd::Tools { refresh, json } => {
            let ctx = Ctx::new(store.clone(), refresh)?;
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
        }
        Cmd::Serve => {
            crate::mcp_server::serve(&store)?;
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
            dry,
        } => match action.as_str() {
            "status" | "st" => println!("{}", crate::git::status()?),
            "main" | "push" => println!(
                "{}",
                crate::git::main_push(message.as_deref(), branch.as_deref(), land, dry)?
            ),
            "tag" => {
                let t = tag
                    .as_deref()
                    .ok_or("git tag 需要 --tag v1.2.3（或用 tba release）")?;
                println!("{}", crate::git::tag_push(t, message.as_deref(), dry, false)?);
            }
            other => {
                return Err(format!(
                    "未知动作: {other}（可选 status | main | tag）"
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
        Cmd::Reload {
            action,
            project,
            root,
            name,
            live,
            args,
            mode,
            compile,
            elevate,
        } => {
            // 参数优先级：命令行 > 项目设置（Teambition 的 setting 任务）> 推断。
            // 项目设置是「这个项目怎么编译/怎么起」的唯一真值，agent 不该每次
            // 重新拼 --root/--name；命令行只用来临时覆盖。
            let pid = project
                .or_else(crate::ops::read_project_binding)
                .unwrap_or_default();
            let st = if pid.is_empty() {
                None
            } else {
                // 先本地缓存（快、离线可用），没有再联网拉一次。
                match crate::setting::ProjectSetting::load_cached(&store, &pid) {
                    Some(s) => Some(s),
                    None => crate::setting::fetch(&store, &pid).ok().flatten(),
                }
            };
            let st = st.unwrap_or_default();
            let root = root
                .filter(|s| !s.trim().is_empty())
                .or_else(|| Some(st.root.clone()).filter(|s| !s.trim().is_empty()))
                .unwrap_or_else(crate::capability::guess_root);
            let name = name
                .filter(|s| !s.trim().is_empty())
                .or_else(|| Some(st.name.clone()).filter(|s| !s.trim().is_empty()))
                .unwrap_or_else(|| crate::capability::guess_name(&root));
            let mode = mode
                .filter(|s| !s.trim().is_empty())
                .or_else(|| Some(st.mode.clone()).filter(|s| !s.trim().is_empty()))
                .unwrap_or_else(|| "rename".to_string());
            let live = live
                .filter(|s| !s.trim().is_empty())
                .or_else(|| Some(st.live.clone()).filter(|s| !s.trim().is_empty()))
                .unwrap_or_else(|| crate::capability::default_live(&root, &name));
            // 重载 TBHub 自己时默认把窗口开回来
            let args = args
                .or_else(|| Some(st.args.clone()).filter(|s| !s.trim().is_empty()))
                .unwrap_or_else(|| {
                    if name == "tba" {
                        "ui".to_string()
                    } else {
                        String::new()
                    }
                });
            let compile = compile.unwrap_or_else(|| st.compile.clone());
            if mode != "rename" && mode != "copy" {
                return Err(format!("未知模式: {mode}（可选 rename|copy）"));
            }
            let req = crate::capability::ReloadReq {
                action: action.clone(),
                root,
                name,
                live,
                args,
                mode,
                compile,
                elevate,
                release_src: false,
            };
            match action.as_str() {
                "status" | "kill" => {
                    let r = crate::capability::run_sync(&req, &store)?;
                    if r.code == 0 {
                        println!("{}", r.out);
                    } else {
                        return Err(fail_msg(&action, &r));
                    }
                }
                "build" | "restart" => {
                    // 自己重载自己：先把 cargo 产物重命名挪开，链接器才写得进去
                    let mut req = req;
                    req.release_src = action == "build" && crate::capability::is_src(&req.root, &req.name);
                    let r = crate::capability::run_sync(&req, &store)?;
                    if r.code == 0 {
                        println!("{}", r.out);
                    } else {
                        return Err(fail_msg(&action, &r));
                    }
                }
                other => {
                    return Err(format!(
                        "未知动作: {other}（可选 build|restart|kill|status）"
                    ))
                }
            }
        }
        Cmd::Setting {
            action,
            project,
            compile,
            root,
            name,
            mode,
            live,
            args,
            on,
            off,
            lang,
        } => {
            let pid = project
                .or_else(crate::ops::read_project_binding)
                .ok_or_else(|| {
                    "没指定项目：加 --project <id>，或在项目目录跑 `tba bind <id>` 记一次绑定"
                        .to_string()
                })?;
            match action.as_str() {
                "show" | "get" => {
                    let st = crate::setting::fetch(&store, &pid)?;
                    match st {
                        Some(s) => {
                            println!("项目设置（{pid}）");
                            println!("  启用        {}", if s.on { "是" } else { "否" });
                            println!("  编译指令    {}", dash(&s.compile));
                            println!("  项目目录    {}", dash(&s.root));
                            println!("  exe 基名    {}", dash(&s.name));
                            println!("  启动模式    {}", dash(&s.mode));
                            println!("  运行副本    {}", dash(&s.live));
                            println!("  启动参数    {}", dash(&s.args));
                            if s.compile.trim().is_empty() {
                                println!(
                                    "\n编译指令是空的：先 `tba setting guess` 看探测结果，\
                                     确认后 `tba setting set --compile \"...\"`。"
                                );
                            }
                        }
                        None => {
                            println!("这个项目还没有设置（没有 status=setting 的任务）。");
                            println!("看探测结果: tba setting guess");
                            println!("存一份:   tba setting set");
                        }
                    }
                }
                "text" | "prompt" => {
                    // 和开工文案同源：agent 收到什么，这里就打印什么。
                    let s = crate::setting::ProjectSetting::load_cached(&store, &pid)
                        .unwrap_or_else(|| crate::setting::ProjectSetting {
                            root: std::env::current_dir()
                                .map(|d| d.display().to_string())
                                .unwrap_or_default(),
                            ..Default::default()
                        });
                    if !s.on {
                        println!("「编译 & 重载」在这个项目里是关的，开工文案里不会出现这一段。");
                        return Ok(());
                    }
                    let zh = lang.as_deref() != Some("en");
                    let exe = crate::ops::prompt_exe(&store);
                    print!("{}", s.prompt_block(&exe, zh));
                }
                "guess" => {
                    let cached = crate::setting::ProjectSetting::load_cached(&store, &pid);
                    let root = root
                        .or_else(|| cached.as_ref().map(|s| s.root.clone()))
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| {
                            std::env::current_dir()
                                .map(|d| d.display().to_string())
                                .unwrap_or_else(|_| ".".to_string())
                        });
                    let mut s = cached.unwrap_or_default();
                    s.root = root;
                    s.fill_defaults();
                    println!("按项目文件探测（还没保存）：");
                    println!("  编译指令    {}", dash(&s.compile));
                    println!("  项目目录    {}", s.root);
                    println!("  exe 基名    {}", s.name);
                    if s.compile.trim().is_empty() {
                        println!(
                            "\n没认出构建文件。看一眼项目根（build.zig / package.json / Makefile / \
                             CMakeLists.txt），确认后用 `tba setting set --compile \"...\"` 写进去。"
                        );
                    } else {
                        println!("\n确认无误就 `tba setting set` 存到 Teambition（status=setting）。");
                    }
                }
                "set" => {
                    // 以云端现有值为底：只改传进来的字段，没传的保持原样。
                    let mut s = crate::setting::fetch(&store, &pid)?
                        .or_else(|| crate::setting::ProjectSetting::load_cached(&store, &pid))
                        .unwrap_or_default();
                    if let Some(v) = compile {
                        s.compile = v;
                    }
                    if let Some(v) = root {
                        s.root = v;
                    }
                    if let Some(v) = name {
                        s.name = v;
                    }
                    if let Some(v) = mode {
                        s.mode = v;
                    }
                    if let Some(v) = live {
                        s.live = v;
                    }
                    if let Some(v) = args {
                        s.args = v;
                    }
                    if on {
                        s.on = true;
                    }
                    if off {
                        s.on = false;
                    }
                    s.fill_defaults();
                    if !s.ready() {
                        return Err(format!(
                            "设置不完整（项目目录「{}」/ exe 名「{}」），补 --root / --name",
                            s.root, s.name
                        ));
                    }
                    println!("{}", crate::setting::save(&store, &pid, &s)?);
                    println!(
                        "  编译指令 {} | 目录 {} | exe {} | 模式 {}",
                        dash(&s.compile),
                        s.root,
                        s.name,
                        s.mode
                    );
                }
                other => {
                    return Err(format!("未知动作: {other}（可选 show|set|guess）"));
                }
            }
        }
    }
    Ok(())
}
