use crate::config::*;
use crate::ops::*;
use crate::store::Store;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn tool_defs() -> Value {
    json!([
        {
            "name": "list_projects",
            "description": "列出 Teambition 企业下所有项目。返回项目 id 和名称。",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "list_project_skills",
            "description": "列出某项目的所有 skill（按任务类型 skill 筛选，不限阶段），含本地开关状态。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string", "description": "项目 ID"}
            }, "required": ["project_id"]}
        },
        {
            "name": "set_switch",
            "description": "打开或关闭某项目的某个 skill。开关只存本地。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string"},
                "skill": {"type": "string", "description": "skill 名称"},
                "on": {"type": "boolean", "description": "true 开，false 关"}
            }, "required": ["project_id", "skill", "on"]}
        },
        {
            "name": "list_tasks",
            "description": "列出某项目指定阶段(或全部)的任务。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string"},
                "stage": {"type": "string", "description": "阶段名。留空列全部。可选: 未完成/进行中/待回复/已解决/已完成/skill/mem"}
            }, "required": ["project_id"]}
        },
        {
            "name": "ensure_stages",
            "description": "确保项目有 7 个工作流状态: 未完成/进行中/待回复/已解决/已完成/skill/mem。缺的会自动创建。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string"}
            }, "required": ["project_id"]}
        },
        {
            "name": "claim_task",
            "description": "领取一条「未完成」任务（只从看板列1的队列里领）。默认取创建时间最早的一条。自动把任务转为「进行中」并挪到看板列2，记录 claim_id。幂等。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string"},
                "task_name": {"type": "string", "description": "可选。按标题包含匹配挑选任务。留空取最早。"}
            }, "required": ["project_id"]}
        },
        {
            "name": "take_task",
            "description": "接单：给任务链接(https://www.teambition.com/task/<id>)或任务 id，改成「进行中」并挪到看板列2。任务已完成/已关闭会拒绝。幂等。",
            "inputSchema": {"type": "object", "properties": {
                "target": {"type": "string", "description": "任务链接或任务 id"},
                "agent": {"type": "string", "description": "可选。谁接的。"}
            }, "required": ["target"]}
        },
        {
            "name": "start_task",
            "description": "开工：没有现成任务时新建一条，直接进入「进行中」并挪到看板列2，返回 claim_id。同名任务已有活动 claim 时幂等复用。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string"},
                "title": {"type": "string", "description": "任务标题"},
                "note": {"type": "string", "description": "可选。任务正文（markdown）"},
                "agent": {"type": "string", "description": "可选。谁开工的。"}
            }, "required": ["project_id", "title"]}
        },
        {
            "name": "ask_user",
            "description": "提问：把任务状态改回「未完成」但仍留在看板列2，同时把问题写成任务评论。用户在 Teambition 任务下评论回复。",
            "inputSchema": {"type": "object", "properties": {
                "claim_id": {"type": "string"},
                "question": {"type": "string", "description": "要问用户的话"}
            }, "required": ["claim_id", "question"]}
        },
        {
            "name": "resume_task",
            "description": "继续：读回用户在任务里的评论回复，把状态改回「进行中」（仍在列2）。",
            "inputSchema": {"type": "object", "properties": {
                "claim_id": {"type": "string"}
            }, "required": ["claim_id"]}
        },
        {
            "name": "beat_task",
            "description": "续命：刷新 claim 心跳。长构建/长测试前后调用，避免被判定会话断开而退回队列。",
            "inputSchema": {"type": "object", "properties": {
                "claim_id": {"type": "string"}
            }, "required": ["claim_id"]}
        },
        {
            "name": "release_task",
            "description": "退回队列：状态改「未完成」并挪回看板列1（主动放弃）。",
            "inputSchema": {"type": "object", "properties": {
                "claim_id": {"type": "string"},
                "reason": {"type": "string", "description": "可选。退回原因，会写成任务评论。"}
            }, "required": ["claim_id"]}
        },
        {
            "name": "sweep_tasks",
            "description": "回收心跳超时的会话：把它们的任务退回 未完成 + 列1。",
            "inputSchema": {"type": "object", "properties": {
                "force": {"type": "boolean", "description": "无视超时，强制回收所有进行中的 claim"}
            }}
        },
        {
            "name": "complete_task",
            "description": "把已领取任务转为「已解决」。",
            "inputSchema": {"type": "object", "properties": {
                "claim_id": {"type": "string"}
            }, "required": ["claim_id"]}
        },
        {
            "name": "close_task",
            "description": "把已领取任务转为「已完成」。",
            "inputSchema": {"type": "object", "properties": {
                "claim_id": {"type": "string"}
            }, "required": ["claim_id"]}
        },
        {
            "name": "read_memory",
            "description": "读取某项目的共享记忆(所有文件)。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string"}
            }, "required": ["project_id"]}
        },
        {
            "name": "write_memory",
            "description": "写某项目的共享记忆。给 file 时覆盖该文件，不给则追加到 notes.md。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string"},
                "content": {"type": "string"},
                "file": {"type": "string", "description": "可选。文件名，如 progress.md"}
            }, "required": ["project_id", "content"]}
        },
        {
            "name": "get_agent_prompt",
            "description": "生成给 agent 的调用文案。含已启用的 skill、共享记忆目录、开工/领任务命令。给了 task 就写进文案让 agent 走 start 新建任务；不给则让 agent 去队列领。",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "string"},
                "project_name": {"type": "string", "description": "可选。项目显示名。"},
                "task": {"type": "string", "description": "可选。本次任务正文(markdown)。给了就走 start 新建任务。"},
                "task_title": {"type": "string", "description": "可选。本次任务标题。留空从 task 首行推断。"},
                "lang": {"type": "string", "enum": ["zh", "en"], "description": "可选。文案语言：zh=中文(准本，默认)，en=英文压缩版(省 token)。"}
            }, "required": ["project_id"]}
        }
    ])
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

// 本次 MCP 会话的标识。写进 claim.agent，用来判断「这条任务属于哪个会话」。
// 带上 pid：两个会话同时跑时不会互相续命。
fn session_tag() -> String {
    format!("mcp-{}", std::process::id())
}

fn ok(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn err(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn dispatch(store: &Store, name: &str, args: &Value) -> Value {
    let r: Result<String, String> = (|| match name {
        "list_projects" => {
            let mut ctx = Ctx::new(store.clone(), false)?;
            let projects = ctx.list_projects()?;
            store.save_projects_cache(
                &projects
                    .iter()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect::<Vec<_>>(),
            )?;
            let lines: Vec<String> = projects
                .iter()
                .map(|p| format!("{}\t{}", p.id, p.name))
                .collect();
            Ok(lines.join("\n"))
        }
        "list_project_skills" => {
            let pid = arg_str(args, "project_id")?;
            let skills = list_skills(store, &pid)?;
            let lines: Vec<String> = skills
                .iter()
                .map(|k| {
                    format!(
                        "[{}] {}\t{}",
                        if k.on { "开" } else { "关" },
                        k.name,
                        k.task_id
                    )
                })
                .collect();
            Ok(lines.join("\n"))
        }
        "set_switch" => {
            let pid = arg_str(args, "project_id")?;
            let skill = arg_str(args, "skill")?;
            let on = args.get("on").and_then(|v| v.as_bool()).unwrap_or(true);
            store.set_switch(&pid, &skill, on)?;
            Ok(format!("skill「{skill}」已{}", if on { "开启" } else { "关闭" }))
        }
        "list_tasks" => {
            let pid = arg_str(args, "project_id")?;
            let stage = args.get("stage").and_then(|v| v.as_str());
            let mut ctx = Ctx::new(store.clone(), false)?;
            let stages = ctx.ensure_stages(&pid)?;
            let names: Vec<String> = match stage {
                Some(s) => vec![s.to_string()],
                None => ALL_STAGES.iter().map(|s| s.to_string()).collect(),
            };
            let mut out = String::new();
            for n in names {
                let st = ctx
                    .find_stage(&stages, &n)
                    .ok_or_else(|| format!("缺少「{n}」阶段"))?;
                let tasks = ctx.list_tasks(&pid, st)?;
                out.push_str(&format!("== {n} ({}) ==\n", tasks.len()));
                for t in &tasks {
                    out.push_str(&format!("  {}\t{}\t{}\n", t.id, t.created, t.title));
                }
            }
            Ok(out)
        }
        "ensure_stages" => {
            let pid = arg_str(args, "project_id")?;
            let mut ctx = Ctx::new(store.clone(), false)?;
            let stages = ctx.ensure_stages(&pid)?;
            let lines: Vec<String> = stages.iter().map(|s| format!("{}\t{}", s.id, s.name)).collect();
            Ok(lines.join("\n"))
        }
        "claim_task" => {
            let pid = arg_str(args, "project_id")?;
            let tn = args.get("task_name").and_then(|v| v.as_str());
            let _ = sweep(store, false);
            let c = claim(store, &pid, tn, &session_tag())?;
            Ok(format!(
                "claim_id: {}\ntask_id: {}\ntask_name: {}\n状态: {} -> {}",
                c.claim_id, c.task_id, c.task_name, c.stage_from, c.stage_to
            ))
        }
        "take_task" => {
            let target = arg_str(args, "target")?;
            let agent = args
                .get("agent")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(session_tag);
            let r = take(store, &target, &agent)?;
            let c = &r.claim;
            let mut out = String::new();
            if r.existed {
                out.push_str("该任务已被领取，复用原 claim（幂等）\n");
            }
            out.push_str(&format!(
                "claim_id: {}\ntask_id: {}\ntask_name: {}\nproject: {}\n状态: {} -> {}\n看板列: {} -> {}\n",
                c.claim_id,
                c.task_id,
                c.task_name,
                c.project_id,
                c.stage_from,
                c.stage_to,
                if c.tasklist_from.is_empty() { "(未知)" } else { &c.tasklist_from },
                r.list_now
            ));
            for w in &r.warnings {
                out.push_str(&format!("警告: {w}\n"));
            }
            Ok(out)
        }
        "start_task" => {
            let pid = arg_str(args, "project_id")?;
            let title = arg_str(args, "title")?;
            let note = args.get("note").and_then(|v| v.as_str()).unwrap_or("");
            let agent = args
                .get("agent")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(session_tag);
            let r = start(store, &pid, &title, note, &agent)?;
            let c = &r.claim;
            let mut out = String::new();
            if r.existed {
                out.push_str("同名任务已开工，复用原 claim（幂等）\n");
            }
            out.push_str(&format!(
                "claim_id: {}\ntask_id: {}\ntask_name: {}\n状态: {} -> {}\n看板列: {} -> {}\n",
                c.claim_id,
                c.task_id,
                c.task_name,
                c.stage_from,
                c.stage_to,
                if c.tasklist_from.is_empty() { "(未知)" } else { &c.tasklist_from },
                r.list_now
            ));
            for w in &r.warnings {
                out.push_str(&format!("警告: {w}\n"));
            }
            Ok(out)
        }
        "ask_user" => {
            let cid = arg_str(args, "claim_id")?;
            let q = arg_str(args, "question")?;
            let c = ask(store, &cid, &q)?;
            Ok(format!(
                "已提问。任务「{}」现在是「未完成」，仍在看板列「{}」等用户回复；问题已写成任务评论。",
                c.task_name,
                store.load_config().tasklist_claim
            ))
        }
        "resume_task" => {
            let cid = arg_str(args, "claim_id")?;
            let (c, replies) = resume(store, &cid)?;
            let mut out = format!(
                "任务「{}」已改回「进行中」，仍在看板列「{}」。\n",
                c.task_name,
                store.load_config().tasklist_claim
            );
            if replies.is_empty() {
                out.push_str("还没读到用户的新评论。");
            } else {
                out.push_str("\n---- 用户回复 ----\n");
                for r in &replies {
                    out.push_str(&format!("[{}] {}\n{}\n", r.created, r.author, r.text));
                }
            }
            Ok(out)
        }
        "beat_task" => {
            let cid = arg_str(args, "claim_id")?;
            let c = beat(store, &cid)?;
            Ok(format!("已续命: {} ({})", c.claim_id, c.state))
        }
        "release_task" => {
            let cid = arg_str(args, "claim_id")?;
            let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            let c = release(store, &cid, reason)?;
            Ok(format!(
                "任务「{}」已退回队列：状态改回「未完成」，看板列请手动拖到「{}」（tba 改不了列）。",
                c.task_name,
                store.load_config().tasklist_queue
            ))
        }
        "sweep_tasks" => {
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            let done = sweep(store, force)?;
            if done.is_empty() {
                Ok("没有需要回收的会话。".to_string())
            } else {
                Ok(done.join("\n"))
            }
        }
        "complete_task" => {
            let cid = arg_str(args, "claim_id")?;
            let c = finish(store, &cid, STAGE_DONE)?;
            Ok(format!(
                "任务「{}」已转为「已解决」。claim_id: {}",
                c.task_name, c.claim_id
            ))
        }
        "close_task" => {
            let cid = arg_str(args, "claim_id")?;
            let c = finish(store, &cid, STAGE_CLOSED)?;
            Ok(format!(
                "任务「{}」已转为「已完成」。claim_id: {}",
                c.task_name, c.claim_id
            ))
        }
        "read_memory" => {
            let pid = arg_str(args, "project_id")?;
            let files = store.read_memory(&pid);
            if files.is_empty() {
                Ok("共享记忆为空。".to_string())
            } else {
                let mut out = String::new();
                for (n, c) in files {
                    out.push_str(&format!("===== {n} =====\n{c}\n"));
                }
                Ok(out)
            }
        }
        "write_memory" => {
            let pid = arg_str(args, "project_id")?;
            let content = arg_str(args, "content")?;
            let file = args.get("file").and_then(|v| v.as_str());
            let p = match file {
                Some(f) => store.write_memory(&pid, Some(f), &content)?,
                None => store.append_memory(&pid, &content)?,
            };
            Ok(format!("已写入 {}", p.display()))
        }
        "get_agent_prompt" => {
            let pid = arg_str(args, "project_id")?;
            let name = args
                .get("project_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| pid.clone());
            // 带了 task 就写进文案，让 agent 走 start 新建任务；不带就去队列领。
            let body = args
                .get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let title = args
                .get("task_title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let draft = if body.trim().is_empty() && title.trim().is_empty() {
                None
            } else {
                let t = if title.trim().is_empty() {
                    body.lines()
                        .find(|l| !l.trim().is_empty())
                        .map(|l| {
                            l.trim()
                                .trim_start_matches("# ")
                                .trim()
                                .chars()
                                .take(60)
                                .collect::<String>()
                        })
                        .unwrap_or_else(|| "本次任务".to_string())
                } else {
                    title
                };
                Some(TaskDraft {
                    title: t,
                    body,
                    // MCP 场景正文直接随文案下发，不落盘。
                    body_path: String::new(),
                })
            };
            // 文案语言：默认中文准本；MCP 调用方可传 "en" 换压缩英文版省 token。
            let lang = args
                .get("lang")
                .and_then(|v| v.as_str())
                .and_then(Lang::parse)
                .unwrap_or_default();
            agent_prompt(store, &pid, &name, draft.as_ref(), lang)
        }
        _ => Err(format!("未知工具: {name}")),
    })();
    match r {
        Ok(t) => ok(t),
        Err(e) => err(e),
    }
}

pub fn serve(store: &Store) -> Result<(), String> {
    let tag = session_tag();
    // 会话心跳：每 30 秒写一次存活标记。进程被会话回收后不再写，
    // sweep 就能判定「这个会话已经没了」。只写独立文件，不碰 claims.jsonl，
    // 避免和状态变更互相覆盖。
    let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let hb_store = store.clone();
        let hb_tag = tag.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            let _ = hb_store.touch_session(&hb_tag);
            while alive.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(30));
                if !alive.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let _ = hb_store.touch_session(&hb_tag);
            }
        });
    }

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(
                    out,
                    "{}",
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("parse error: {e}")}})
                );
                let _ = out.flush();
                continue;
            }
        };
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = v.get("id").cloned();

        // 通知没有 id，不需要响应。
        let need_reply = id.is_some() && !id.as_ref().map(|i| i.is_null()).unwrap_or(true);

        let result: Option<Value> = match method {
            "initialize" => Some(json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "tba", "version": "0.1.0"}
            })),
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({"tools": tool_defs()})),
            "tools/call" => {
                let params = v.get("params").cloned().unwrap_or(json!({}));
                let name = params
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Some(dispatch(store, &name, &args))
            }
            "notifications/initialized" | "notifications/cancelled" | "initialized" => None,
            _ => {
                if need_reply {
                    Some(json!({"error": {"code": -32601, "message": format!("method not found: {method}")}}))
                } else {
                    None
                }
            }
        };

        if need_reply {
            if let Some(r) = result {
                // 已经是完整错误对象的不再包 result
                let msg = if r.get("error").is_some() {
                    json!({"jsonrpc":"2.0","id":id,"error":r.get("error")})
                } else {
                    json!({"jsonrpc":"2.0","id":id,"result":r})
                };
                let _ = writeln!(out, "{}", msg);
                let _ = out.flush();
            }
        }
    }

    // 会话正常结束：停心跳、删标记，并把本会话还占着的（running）任务退回队列。
    // waiting 的不动——那是在等用户回复，会话结束也该继续等。
    alive.store(false, std::sync::atomic::Ordering::Relaxed);
    store.drop_session(&tag);
    let mine: Vec<crate::store::Claim> = store
        .active_claims()
        .into_iter()
        .filter(|c| c.agent == tag && c.is_running())
        .collect();
    for c in mine {
        let _ = release(
            store,
            &c.claim_id,
            "MCP 会话结束（窗口关闭 / 进程退出），任务退回队列",
        );
    }
    Ok(())
}
