use crate::config::*;
use crate::ops::*;
use crate::store::Store;
use crate::Cmd;
use clap::Parser;

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

pub fn run() -> Result<(), String> {
    let store = Store::new();
    store.ensure_dirs()?;
    let cli = crate::Cli::parse();

    // 任何命令启动都顺带给「本会话占着的 running 任务」刷一次心跳：
    // 会话还活着（还在跑 tb）就不该被当成僵尸回收，长构建期间尤其需要。
    // sweep / hello 除外——sweep 是来回收的，先续命就回收不掉了。
    if !matches!(&cli.cmd, Some(Cmd::Sweep { .. }) | Some(Cmd::Hello { .. })) {
        store.beat_agent(&current_agent());
    }
    // 不带子命令（双击 tb / 直接 tb）→ 打开 GUI 窗口。agent 调用一律走显式子命令(CLI)。
    let cmd = cli.cmd.unwrap_or(Cmd::Ui);
    match cmd {
        Cmd::Login { token } => {
            crate::oauth::login(&store, token)?;
        }
        Cmd::Logout => {
            crate::oauth::logout(&store)?;
        }
        Cmd::Config { show, url } => {
            let mut cfg = store.load_config();
            let switched = url.is_some();
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
                println!("\n配置文件: {}", store.root.join("config.toml").display());
            }
        }
        Cmd::Projects => {
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
            println!("项目 {project} 的阶段（工作流状态，已自动补齐）:");
            for s in &stages {
                println!("  {}\t{}", s.id, s.name);
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
                println!("      列 1 = 待处理  （队列：没人接的任务）");
                println!("      列 2 = 进行中  （agent 正在干）");
                println!("      列 3 = 待回复  （等用户在 Teambition 评论回复）");
                println!("      列 4 = 已解决  （已完成）");
                println!("      列 5 = 关闭    （作废）");
                println!("    建好后重跑一次 tb init 校验。");
            }
        }
        Cmd::Skills { project } => {
            let skills = list_skills(&store, &project)?;
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
            let skills = list_skills(&store, &project)?;
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
        Cmd::Claim { project, name } => {
            // 领之前先回收死会话，别让一个已经不存在的会话占着任务。
            for m in sweep(&store, false).unwrap_or_default() {
                eprintln!("{m}");
            }
            let c = claim(&store, &project, name.as_deref(), &current_agent())?;
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
            println!("\n接单完成。需要用户回复时用: tb ask {} --question \"...\"", c.claim_id);
            println!("干完用: tb done {}  |  放弃用: tb release {}", c.claim_id, c.claim_id);
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
            println!("\n开工完成。需要用户回复时用: tb ask {} --question \"...\"", c.claim_id);
            println!("干完用: tb done {}  |  放弃用: tb release {}", c.claim_id, c.claim_id);
        }
        Cmd::Ask { claim_id, question } => {
            let c = ask(&store, &claim_id, &question)?;
            let cfg = store.load_config();
            println!(
                "已提问。任务「{}」现在是「{}」，看板列「{}」，球在你那边。",
                c.task_name, STAGE_WAIT, cfg.tasklist_wait
            );
            println!("问题已写成任务评论，在 Teambition 任务下回复即可。");
            println!(
                "\n接着等回复: tb wait {}   （阻塞轮询，你回一句它就返回正文并继续，\n\
                 不用再回到会话里发消息。单轮默认 540 秒，超时退出码 2，再跑一轮即可。）",
                c.claim_id
            );
            println!("不想等只想看一眼: tb reply {}", c.claim_id);
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
                "\n之后本目录下所有 tb 命令自动用这个标识（也可以用 TB_AGENT 环境变量覆盖）。\n\
                 同一目录要跑多个会话，用 tb hello --name wb-a / --name wb-b 分别指定，\n\
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
                    println!("• {}  [{}]", c.task_name, c.claim_id);
                    println!("  等了 {ago}  ·  会话 {}", c.agent);
                    if !c.question.trim().is_empty() {
                        println!("  问题: {}", c.question.lines().next().unwrap_or(""));
                    }
                    println!("  {}", task_url(&c.task_id));
                    println!();
                }
                println!("在 Teambition 上回复后，agent 侧执行 tb wait/tb reply <claim_id> 继续。");
            }
        }
        Cmd::Wait {
            claim_id,
            timeout,
            interval,
        } => {
            let t = timeout.unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS);
            let iv = interval.unwrap_or(DEFAULT_WAIT_INTERVAL_SECS);
            let cfg = store.load_config();
            match wait(&store, &claim_id, t, iv)? {
                WaitOutcome::Replied {
                    claim: c,
                    replies,
                    waited_secs,
                } => {
                    println!(
                        "收到回复（等了 {waited_secs} 秒）。任务「{}」已回到「{}」+ 看板列「{}」。",
                        c.task_name, STAGE_DOING, cfg.tasklist_claim
                    );
                    println!("\n---- 你的回复 ----");
                    for r in &replies {
                        println!("[{}] {}\n{}", r.created, r.author, r.text);
                    }
                    println!("\n接着干。干完用: tb done {}", c.claim_id);
                }
                WaitOutcome::Timeout {
                    claim: c,
                    waited_secs,
                } => {
                    println!(
                        "等了 {waited_secs} 秒还没收到回复。任务「{}」仍在「{}」+ 看板列「{}」，球在你那边。",
                        c.task_name, STAGE_WAIT, cfg.tasklist_wait
                    );
                    println!("继续等就再跑一次: tb wait {}", c.claim_id);
                    // 退出码 2 = 超时（没回复），agent 据此决定是否再等一轮。
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
        Cmd::Beat { claim_id } => {
            let c = beat(&store, &claim_id)?;
            println!("已续命: {} ({})", c.claim_id, c.state);
        }
        Cmd::Release { claim_id, reason } => {
            let c = release(&store, &claim_id, reason.as_deref().unwrap_or(""))?;
            println!("任务「{}」已退回队列（待处理 + 列{}）。", c.task_name,
                store.load_config().tasklist_queue);
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
            println!("任务「{}」已转为「关闭」。claim_id: {}", c.task_name, c.claim_id);
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
                    "closed" => "关闭".to_string(),
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
            task,
            task_file,
        } => {
            let mut ctx = Ctx::new(store.clone(), false)?;
            let projects = ctx.list_projects().unwrap_or_default();
            let name = projects
                .iter()
                .find(|p| p.id == project)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| project.clone());

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
            print!("{}", agent_prompt(&store, &project, &name, draft.as_ref())?);
        }
        Cmd::Memory { project, write, file } => {
            match write {
                Some(text) => match file {
                    Some(f) => {
                        let p = store.write_memory(&project, Some(&f), &text)?;
                        println!("已写入 {}", p.display());
                    }
                    None => {
                        let p = store.append_memory(&project, &text)?;
                        println!("已追加 {}", p.display());
                    }
                },
                None => {
                    let files = store.read_memory(&project);
                    if files.is_empty() {
                        println!("共享记忆为空。目录: {}", store.memory_dir(&project).display());
                    }
                    for (name, content) in files {
                        println!("===== {name} =====");
                        println!("{content}");
                    }
                }
            }
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
        Cmd::Reload {
            action,
            root,
            name,
            live,
            args,
            mode,
            elevate,
        } => {
            let root = root.unwrap_or_else(crate::capability::guess_root);
            let name = name.unwrap_or_else(|| crate::capability::guess_name(&root));
            let live = live.unwrap_or_else(|| crate::capability::default_live(&root, &name));
            // 重载 TBHub 自己时默认把窗口开回来
            let args = args.unwrap_or_else(|| {
                if name == "tb" {
                    "ui".to_string()
                } else {
                    String::new()
                }
            });
            let mode = mode.unwrap_or_else(|| "rename".to_string());
            if mode != "rename" && mode != "copy" {
                return Err(format!("未知模式: {mode}（可选 rename|copy）"));
            }
            match action.as_str() {
                "status" | "kill" => {
                    let r = crate::capability::run_sync(
                        &action, &root, &name, &live, &args, &mode, elevate, false,
                    )?;
                    if r.code == 0 {
                        println!("{}", r.out);
                    } else {
                        return Err(fail_msg(&action, &r));
                    }
                }
                "build" | "restart" => {
                    // 自己重载自己：先把 cargo 产物重命名挪开，链接器才写得进去
                    let release_src = action == "build" && crate::capability::is_src(&root, &name);
                    let r = crate::capability::run_sync(
                        &action,
                        &root,
                        &name,
                        &live,
                        &args,
                        &mode,
                        elevate,
                        release_src,
                    )?;
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
    }
    Ok(())
}
