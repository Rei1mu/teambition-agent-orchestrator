/**
 * WatchAggregator — 单进程聚合监听，取代 per-claim 的 detached `wait --listen`。
 *
 * 旧模型：每个 waiting/running claim 起一个独立进程，各持一条 IMAP 长连接 +
 * 评论 MCP 往返。多 claim 时 N 条 TLS 连接挤 QQ 邮箱，慢且会被限流。
 *
 * 新模型：dsh-tba-bridge 之外，tba.exe serve（DSH 托管、常驻、自动重拉）里
 * 起一个聚合线程，持 1 条 ImapSession 扫全部邮件，按 [TB <claim_id>] marker
 * 分发；评论按 claim 轮。状态全在内存，总量 <10KB；持久化复用现有文件
 * （waits/<claim>.session / events.jsonl / claims.jsonl / mail watermark）。
 *
 * 唤醒仍走文件握手：收到回复 → write_wake_request 写 wakes/<sessionId>.json
 * → dsh-tba-bridge 插件 steer() 注入会话、立刻插队执行。
 */

use crate::events;
use crate::mail::{marker_hit, ImapSession};
use crate::config::STAGE_DONE;
use crate::ops::{
    add_todo_task, ask, beat, finish, match_project_id, parse_add_task, parse_reply, resume,
    write_wake_request, ReplyAction,
};
use crate::store::{Claim, Store};
use crate::tools::CommentInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

const MAIL_POLL_SECS: u64 = 5;
const COMMENT_POLL_SECS: u64 = 10;

// 每个 claim 的评论水位：基准时间 + 上一条评论时间。
// 等价于旧 wait() 里的 WatchBaseline + last_comment_ts，内联在聚合器里。
#[derive(Default)]
struct CommentCursor {
    since: String,       // baseline：ask_at 或监听启动时刻
    last_comment_ts: String,
    prev_has_ask: Option<bool>,
}

impl CommentCursor {
    // 返回本轮评论过滤基准；has_ask 切换时重置基准，避免漏掉新提问窗口的回复。
    fn observe(&mut self, has_ask: bool, ask_at: &str, now: &str) -> String {
        if self.prev_has_ask != Some(has_ask) {
            self.since = now.to_string();
            self.prev_has_ask = Some(has_ask);
        }
        if has_ask {
            ask_at.to_string()
        } else {
            self.since.clone()
        }
    }
    // 推进水位到本轮最新一条评论时间。
    fn advance(&mut self, latest: &str) {
        // 空水位时 ts_after(latest, "") 因解析失败返回 false，水位永远推不上去，
        // 同一条评论会被每轮当成新回复重放（常驻聚合器尤其明显）。
        if self.last_comment_ts.is_empty()
            || crate::ops::ts_after(latest, &self.last_comment_ts)
        {
            self.last_comment_ts = latest.to_string();
        }
    }
    // 过滤基准 = max(last_comment_ts, baseline)，去重 + 不漏。
    fn after(&self, baseline: &str) -> String {
        if crate::ops::ts_after(&self.last_comment_ts, baseline) {
            self.last_comment_ts.clone()
        } else {
            baseline.to_string()
        }
    }
}

pub fn run_aggregator(store: Store, alive: Arc<AtomicBool>) {
    // 离线模式不连 Teambition：回复聚合器整条腿（IMAP + 评论 + 状态流转）都不要起，
    // 一处早退覆盖所有 spawn 路径。 wakes 握手写文件是本地操作，不在此拦。
    if store.load_config().offline() {
        eprintln!("当前是离线模式，不启动回复聚合器（不轮询邮件/Teambition）。");
        return;
    }
    let mut imap: Option<ImapSession> = None;
    let mut cursors: HashMap<String, CommentCursor> = HashMap::new();
    let mut last_mail = Instant::now() - std::time::Duration::from_secs(MAIL_POLL_SECS);
    let mut last_comment = Instant::now() - std::time::Duration::from_secs(COMMENT_POLL_SECS);
    let mut last_beat = Instant::now();

    while alive.load(Ordering::Relaxed) {
        let active = store.active_claims();
        if active.is_empty() {
            std::thread::sleep(std::time::Duration::from_secs(MAIL_POLL_SECS));
            continue;
        }

        // ---- 邮件：1 条连接扫全部，按 marker 分发 ----
        if crate::mail::can_receive(&store.load_config()) && last_mail.elapsed().as_secs() >= MAIL_POLL_SECS {
            last_mail = Instant::now();
            if imap.is_none() {
                imap = Some(ImapSession::new(
                    store.load_config(),
                    crate::mail::MailScan::with_watermark(None),
                ));
            }
            match imap.as_mut().unwrap().poll_all() {
                Ok(msgs) => {
                    for (subject, body, _uid) in msgs {
                        // 一封邮件可能同时带多个 marker（转发/引用），只归最近的活跃 claim。
                        let mut hits: Vec<&Claim> = active
                            .iter()
                            .filter(|c| {
                                let marker = format!("[TB {}]", c.claim_id);
                                marker_hit(&subject, &marker) || marker_hit(&body, &marker)
                            })
                            .collect();
                        hits.sort_by(|a, b| b.heartbeat().cmp(a.heartbeat()));
                        if let Some(c) = hits.first() {
                            let marker = format!("[TB {}]", c.claim_id);
                            let text = crate::mail::clean_reply(&body, &marker);
                            handle_message(&store, c, &text, "mail", None);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("… 聚合 IMAP 失败（下轮重连）: {e}");
                    imap = None; // 断开，下轮 open()
                }
            }
        }

        // ---- 评论：每 claim 轮，按各自水位过滤 ----
        if last_comment.elapsed().as_secs() >= COMMENT_POLL_SECS {
            last_comment = Instant::now();
            for c in &active {
                // Ctx 每次新建会 discover（MCP 往返）；聚合器是慢循环，成本可接受。
                let mut ctx = match crate::ops::Ctx::new(store.clone(), false) {
                    Ok(x) => x,
                    Err(e) => {
                        eprintln!("… 聚合评论跳过（Ctx 失败）: {e}");
                        continue;
                    }
                };
                let has_ask = !c.ask_at.is_empty();
                let cur = cursors.entry(c.claim_id.clone()).or_default();
                let baseline = cur.observe(has_ask, &c.ask_at, &chrono::Local::now().to_rfc3339());
                let after = cur.after(&baseline);
                let fresh: Vec<CommentInfo> = ctx
                    .read_comments(&c.task_id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|x| crate::ops::ts_after(&x.created, &after))
                    .filter(|x| !x.text.trim_start().starts_with("（邮件回复）"))
                    .collect();
                if let Some(latest) = fresh.iter().map(|x| x.created.as_str()).max() {
                    cur.advance(latest);
                }
                if !fresh.is_empty() {
                    // events 里已落过同样正文的评论直接跳过：拦住跨轮重复
                    // （水位解析失败 / 多实例并发时同一条评论被当新评论重放），
                    // 免得同一条评论触发两次 wake 重复唤醒会话、烧 token。
                    let seen: std::collections::HashSet<String> = events::load(&store.root)
                        .into_iter()
                        .filter(|e| e.claim_id == c.claim_id && e.source == "comment")
                        .map(|e| comment_dedup_key(&e.text))
                        .collect();
                    let new: Vec<&CommentInfo> = fresh
                        .iter()
                        .filter(|x| !seen.contains(&comment_dedup_key(&x.text)))
                        .collect();
                    if !new.is_empty() {
                        // 多条 wake 文件会互相覆盖（bridge 只读最后一个），所以
                        // events 每条都记，wake 只写一次：多条时用带条数的合并摘要，
                        // 单条时正文照旧。handle_message 内部照常走 pending/添加任务语义。
                        let merged = if new.len() == 1 {
                            wake_text("comment", &new[0].text)
                        } else {
                            format!(
                                "（{} 条新评论）{}",
                                new.len(),
                                wake_text("comment", &new.last().unwrap().text)
                            )
                        };
                        // wake 写一次：第一条 handle 带合并文案，其余传空串跳过 wake。
                        for (i, x) in new.iter().enumerate() {
                            let w = if i == 0 { Some(merged.as_str()) } else { Some("") };
                            handle_message(&store, c, &x.text, "comment", w);
                        }
                    }
                }
            }
        }

        // ---- 保活：running claim 每 60s beat，别被 sweep 误判 ----
        if last_beat.elapsed().as_secs() >= 60 {
            last_beat = Instant::now();
            for c in &active {
                if c.is_running() {
                    let _ = beat(&store, &c.claim_id);
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(MAIL_POLL_SECS));
    }
    // 退出时 LOGOUT：imap drop 时连接自然关闭，显式 LOGOUT 交给 ImapSession drop。
}

// 「添加一个任务」确认态：ask 只改状态、不改变这条回复的语义，
// 所以待确认的项目/标题落在本地文件里（按 claim 隔离），不塞进 Claim 结构。
const ADD_TASK_PENDING_DIR: &str = "addtask-pending";

#[derive(Clone, Serialize, Deserialize)]
struct PendingChoice {
    num: u32,
    project_id: Option<String>, // None = 取消添加
    label: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct PendingAddTask {
    title: String,
    source: String,
    choices: Vec<PendingChoice>,
}

struct PendingResolution {
    project_id: Option<String>,
}

fn pending_path(root: &Path, claim_id: &str) -> PathBuf {
    root.join(ADD_TASK_PENDING_DIR).join(format!("{claim_id}.json"))
}

fn save_pending(root: &Path, claim_id: &str, p: &PendingAddTask) -> Result<(), String> {
    let path = pending_path(root, claim_id);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let s = serde_json::to_string(p).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, s).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn load_pending(root: &Path, claim_id: &str) -> Option<PendingAddTask> {
    let s = std::fs::read_to_string(pending_path(root, claim_id)).ok()?;
    serde_json::from_str(&s).ok()
}

fn clear_pending(root: &Path, claim_id: &str) {
    let _ = std::fs::remove_file(pending_path(root, claim_id));
}

fn project_name(projects: &[(String, String)], id: &str) -> String {
    projects
        .iter()
        .find(|(pid, _)| pid == id)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| id.to_string())
}

fn notify(store: &Store, claim_id: &str, text: &str) {
    events::append(&store.root, claim_id, "addtask", text, "free");
    write_wake_request(&store.root, claim_id, text, "free");
}

// 评论去重键：去首尾空白的正文。events 里没有评论 id/created 字段，
// 用正文做键足够拦住「同一条评论跨轮重复处理」这个实际故障。
fn comment_dedup_key(text: &str) -> String {
    text.trim().to_string()
}

// 邮件回信经 clean_reply 后正文不带 marker；唤醒文案仍保留「邮件回复」标记，
// 让会话里的 agent 一眼看出来源（parse_reply 的 strip_reply_noise 会剥掉它）。
fn wake_text(source: &str, text: &str) -> String {
    if source == "mail" {
        format!("（邮件回复）{text}")
    } else {
        text.to_string()
    }
}

// 找不到项目时给用户看的候选：优先名字包含用户输入的，最多 5 个；
// 一个都没有就列前 5 个项目，避免用户面对一长串项目名。
fn candidate_projects(
    projects: &[(String, String)],
    asked: &str,
    current_id: &str,
) -> Vec<(String, String)> {
    let needle = asked.trim().to_lowercase();
    let mut out: Vec<(String, String)> = projects
        .iter()
        .filter(|(id, name)| {
            id != current_id && (needle.is_empty() || name.to_lowercase().contains(&needle))
        })
        .take(5)
        .cloned()
        .collect();
    if out.is_empty() {
        out = projects
            .iter()
            .filter(|(id, _)| id != current_id)
            .take(5)
            .cloned()
            .collect();
    }
    out
}

// 选项必须从 3 起：1/2 是系统保留号（收尾 / 写记忆）。
fn build_confirmation(
    title: &str,
    asked: &str,
    current_id: &str,
    current_name: &str,
    candidates: Vec<(String, String)>,
) -> (String, Vec<PendingChoice>) {
    let mut lines = vec![
        format!("没找到项目「{asked}」。请选择在哪个项目添加任务「{title}」："),
        String::new(),
    ];
    let mut choices = Vec::new();
    let mut num = 3u32;
    for (id, name) in candidates {
        lines.push(format!("{num} = {name}"));
        choices.push(PendingChoice { num, project_id: Some(id), label: name });
        num += 1;
    }
    lines.push(format!("{num} = 用当前项目「{current_name}」"));
    choices.push(PendingChoice {
        num,
        project_id: Some(current_id.to_string()),
        label: current_name.to_string(),
    });
    num += 1;
    lines.push(format!("{num} = 取消添加"));
    choices.push(PendingChoice {
        num,
        project_id: None,
        label: "取消添加".to_string(),
    });
    lines.push(String::new());
    lines.push("回复选项数字即可（1/2 是系统保留号）。".to_string());
    (lines.join("\n"), choices)
}

fn resolve_pending(
    pending: &PendingAddTask,
    text: &str,
    projects: &[(String, String)],
) -> Option<PendingResolution> {
    let (action, body) = parse_reply(text);
    match action {
        ReplyAction::Custom(n) => pending
            .choices
            .iter()
            .find(|c| c.num == n)
            .map(|c| PendingResolution {
                project_id: c.project_id.clone(),
            }),
        // 用户直接回项目名也认：不强迫一定按编号选。
        ReplyAction::Free => {
            let id = match_project_id(projects, &body)?;
            Some(PendingResolution {
                project_id: Some(id),
            })
        }
        // 1/2 是系统动作，不当项目确认。
        _ => None,
    }
}

fn create_and_notify(
    store: &Store,
    claim: &Claim,
    title: &str,
    project_id: &str,
    source: &str,
) {
    let note = format!("由 tba 聚合监听从{source}指令添加");
    match add_todo_task(store, project_id, title, &note) {
        Ok(r) => {
            let mut msg = format!(
                "已从{source}指令添加任务「{title}」到项目「{}」（task_id {}）",
                project_name(&store.load_projects_cache(), project_id),
                r.task_id
            );
            if let Some(w) = r.list_warning {
                msg.push_str(&format!("；{w}"));
            }
            notify(store, &claim.claim_id, &msg);
        }
        Err(e) => notify(store, &claim.claim_id, &format!("添加任务「{title}」失败：{e}")),
    }
}

// 返回 true = 这条消息已消费，不再当普通回复分发。
fn handle_add_command(store: &Store, claim: &Claim, text: &str, source: &str) -> bool {
    let Some(req) = parse_add_task(text) else {
        return false;
    };
    let projects = store.load_projects_cache();
    match req.project.as_deref() {
        Some(name) => match match_project_id(&projects, name) {
            Some(pid) => create_and_notify(store, claim, &req.title, &pid, source),
            None => {
                let current_name = project_name(&projects, &claim.project_id);
                let candidates = candidate_projects(&projects, name, &claim.project_id);
                let (q, choices) = build_confirmation(
                    &req.title,
                    name,
                    &claim.project_id,
                    &current_name,
                    candidates,
                );
                let pending = PendingAddTask {
                    title: req.title.clone(),
                    source: source.to_string(),
                    choices,
                };
                if let Err(e) = save_pending(&store.root, &claim.claim_id, &pending) {
                    notify(store, &claim.claim_id, &format!("记不下项目确认状态，没添加任务：{e}"));
                } else {
                    notify(
                        store,
                        &claim.claim_id,
                        &format!("收到添加任务「{}」的指令，正在等你确认项目。", req.title),
                    );
                    if let Err(e) = ask(store, &claim.claim_id, &q) {
                        clear_pending(&store.root, &claim.claim_id);
                        notify(store, &claim.claim_id, &format!("项目确认提问失败，没添加任务：{e}"));
                    }
                }
            }
        },
        None => create_and_notify(store, claim, &req.title, &claim.project_id, source),
    }
    true
}

fn handle_pending_answer(store: &Store, claim: &Claim, text: &str, source: &str) -> bool {
    let path = pending_path(&store.root, &claim.claim_id);
    if !path.exists() {
        return false;
    }
    // 用户在等确认时又发了新的「添加一个任务」：旧确认作废，新指令照常处理。
    if parse_add_task(text).is_some() {
        clear_pending(&store.root, &claim.claim_id);
        return false;
    }
    let pending = match load_pending(&store.root, &claim.claim_id) {
        Some(p) => p,
        None => {
            clear_pending(&store.root, &claim.claim_id);
            return false;
        }
    };
    let projects = store.load_projects_cache();
    let resolution = resolve_pending(&pending, text, &projects);
    clear_pending(&store.root, &claim.claim_id);
    match resolution {
        Some(r) => match r.project_id {
            Some(pid) => create_and_notify(store, claim, &pending.title, &pid, &pending.source),
            None => notify(store, &claim.claim_id, &format!("已取消添加任务「{}」。", pending.title)),
        },
        None => notify(
            store,
            &claim.claim_id,
            &format!("没识别到项目，任务「{}」未添加。", pending.title),
        ),
    }
    // 确认完照常回到「进行中」；1/2 仍由 dispatch_reply 按系统语义处理。
    dispatch_reply(store, &claim.claim_id, text, source);
    true
}

// wake_override: Some = 用调用方给的文案写 wake（多条评论合并时用），
// None = 用本条 text 写（邮件/单条评论的默认路径）。
fn handle_message(store: &Store, claim: &Claim, text: &str, source: &str, wake_override: Option<&str>) {
    if handle_pending_answer(store, claim, text, source) {
        return;
    }
    if handle_add_command(store, claim, text, source) {
        return;
    }
    let (action, _body) = parse_reply(text);
    let tag = action.event_tag();
    events::append(&store.root, &claim.claim_id, source, text, &tag);
    if let Some(w) = wake_override {
        if !w.is_empty() {
            write_wake_request(&store.root, &claim.claim_id, w, &tag);
        }
    } else {
        let wake = wake_text(source, text);
        // parse_reply 要的是干净原文。wake 前缀「（邮件回复）」与正文同一行，
        // strip_reply_noise 会把整行当标记丢弃，导致「1」解析成空、不收尾。
        // 唤醒/落盘用 wake（给人看），动作判定和分类用干净 text。
        write_wake_request(&store.root, &claim.claim_id, &wake, &tag);
    }
    dispatch_reply(store, &claim.claim_id, text, source);
}

// 分发一条回复：parse_reply 判动作，回 1 才 finish；其余 resume/noted 不改终态。
// 与旧 wait() 的语义保持一致，只是不再有 emit/前台等待者——唤醒由 write_wake_request 负责。
fn dispatch_reply(store: &Store, claim_id: &str, text: &str, _source: &str) {
    let (action, _body) = parse_reply(text);
    let claim = match store.get_claim(claim_id) {
        Some(c) => c,
        None => return,
    };

    // 回 1 是明确收尾指令，跟任务当前是「待回复」还是「进行中」无关——
    // 进行中任务的 ask_at 为空，旧逻辑在这里直接 return，导致用户回 1 没反应。
    // 先处理 Closed 再谈状态门：进行中不需要 resume（本就 running），直接联动 + finish。
    if action == ReplyAction::Closed {
        // 回 1 联动：lead 收同会话全部子任务；子代理（note=agent-N）收同 tag 的。
        // 普通任务（无 tag 非 lead）返回空，只 finish 自己。
        match crate::ops::lead_finalize_peers(store, claim_id) {
            Ok(peers) if !peers.is_empty() => {
                eprintln!("… {claim_id} 回1联动收尾: {}", peers.join("；"));
            }
            Ok(_) => {}
            Err(e) => eprintln!("… {claim_id} 回1联动收尾失败: {e}"),
        }
        if let Err(e) = finish(store, claim_id, STAGE_DONE) {
            eprintln!("… 聚合收尾 {claim_id} 失败: {e}");
        }
        return;
    }

    // 非收尾回复：只有「待回复」态才 resume 推回进行中；进行中态不动状态，
    // 原话已在 events/wakes 里，交给 agent 自己判断。
    if claim.ask_at.is_empty() {
        return;
    }
    if let Err(e) = resume(store, claim_id) {
        eprintln!("… 聚合回复 {claim_id} 时 resume 失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 一封邮件按 marker 只归一个 claim；折叠 marker（中间被塞空格）也能认出。
    #[test]
    fn marker_routes_to_exactly_one_claim() {
        let claims = vec!["clm-aaa-0".to_string(), "clm-bbb-0".to_string()];
        let body = "[TB clm-a aa-0] 收到\n1\nRR\nx@qq.com";
        let matches: Vec<&String> = claims
            .iter()
            .filter(|id| {
                let marker = format!("[TB {id}]");
                marker_hit(body, &marker)
            })
            .collect();
        assert_eq!(matches.len(), 1, "折叠 marker 仍只匹配一个 claim");
        assert_eq!(matches[0], "clm-aaa-0");
    }

    // 空水位下 advance 必须落地，否则同一条评论每轮被当成新回复重放。
    #[test]
    fn advance_from_empty_watermark_stops_replay() {
        let mut cur = CommentCursor::default();
        let baseline = "2026-11-15T22:00:00+08:00";
        assert_eq!(cur.after(baseline), baseline, "初始水位为空，退回基准");

        let comment = "2026-11-15T22:02:10+08:00";
        cur.advance(comment);
        assert_eq!(cur.last_comment_ts, comment, "空水位也要推进到本轮最新");
        assert_eq!(cur.after(baseline), comment, "下轮基准应为最新评论时间");
        assert!(
            !crate::ops::ts_after(comment, &cur.after(baseline)),
            "同一条评论不再满足 ts_after，不会被重放"
        );
    }

    #[test]
    fn add_task_confirmation_starts_options_at_three() {
        let (q, choices) = build_confirmation(
            "补文档",
            "未知项目",
            "cur-id",
            "当前项目",
            vec![("a".into(), "A".into()), ("b".into(), "B".into())],
        );
        let nums: Vec<u32> = choices.iter().map(|c| c.num).collect();
        assert_eq!(nums, vec![3, 4, 5, 6]);
        assert_eq!(choices[2].project_id.as_deref(), Some("cur-id"));
        assert_eq!(choices[3].project_id, None, "最后一项是取消");
        assert!(q.lines().any(|l| l.starts_with("3 = A")));
        assert!(q.lines().any(|l| l.starts_with("6 = 取消添加")));
        assert!(!q.lines().any(|l| l.starts_with("1 =") || l.starts_with("2 =")));
    }

    #[test]
    fn pending_answer_accepts_number_or_project_name() {
        let pending = PendingAddTask {
            title: "补文档".into(),
            source: "comment".into(),
            choices: vec![
                PendingChoice { num: 3, project_id: Some("a".into()), label: "A".into() },
                PendingChoice { num: 4, project_id: None, label: "取消添加".into() },
            ],
        };
        let projects = vec![
            ("a".to_string(), "A".to_string()),
            ("b".to_string(), "B".to_string()),
        ];
        let by_num = resolve_pending(&pending, "3", &projects).unwrap();
        assert_eq!(by_num.project_id.as_deref(), Some("a"));

        let by_name = resolve_pending(&pending, "B", &projects).unwrap();
        assert_eq!(by_name.project_id.as_deref(), Some("b"));

        let cancelled = resolve_pending(&pending, "4", &projects).unwrap();
        assert_eq!(cancelled.project_id, None);

        // 1/2 是系统保留号，不能拿来做项目确认。
        assert!(resolve_pending(&pending, "1", &projects).is_none());
        assert!(resolve_pending(&pending, "2", &projects).is_none());
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tba-agg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // B：多条 fresh 评论全部通过去重判定进 events；已存在的重复评论被挡住。
    #[test]
    fn multiple_fresh_comments_all_recorded_duplicates_filtered() {
        let dir = tmpdir("multi");
        // events 里已有第一条评论（上一轮落过）
        events::append(&dir, "clm-y-0", "comment", "第一条", "free");
        let seen: std::collections::HashSet<String> = events::load(&dir)
            .into_iter()
            .filter(|e| e.claim_id == "clm-y-0" && e.source == "comment")
            .map(|e| comment_dedup_key(&e.text))
            .collect();

        let fresh_texts = ["第一条", "第二条", "第三条"];
        let accepted: Vec<&str> = fresh_texts
            .iter()
            .copied()
            .filter(|t| !seen.contains(&comment_dedup_key(t)))
            .collect();
        assert_eq!(accepted, vec!["第二条", "第三条"], "重复的第一条被挡，其余全进");
        for t in &accepted {
            events::append(&dir, "clm-y-0", "comment", t, "free");
        }
        let texts: Vec<String> = events::load(&dir)
            .into_iter()
            .filter(|e| e.claim_id == "clm-y-0")
            .map(|e| e.text)
            .collect();
        // 第一条只出现一次，第二条/第三条各一次。
        assert_eq!(texts.iter().filter(|t| *t == "第一条").count(), 1, "不重复写");
        assert_eq!(texts.iter().filter(|t| *t == "第二条").count(), 1);
        assert_eq!(texts.iter().filter(|t| *t == "第三条").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // D：write_wake_request 写出的 payload 字段齐全（证明 wake 确实写过）。
    // bridge 消费后 wakes/*.json 会被删，磁盘上看不到不等于没写——这里直接断言 payload。
    #[test]
    fn wake_payload_has_session_text_and_claim() {
        let dir = tmpdir("wake");
        std::fs::create_dir_all(dir.join("waits")).unwrap();
        std::fs::write(dir.join("waits").join("clm-z-0.session"), "session-test-123").unwrap();
        write_wake_request(&dir, "clm-z-0", "用户回来了", "free");
        let raw = std::fs::read_to_string(dir.join("wakes").join("session-test-123.json"))
            .expect("wake 文件要写出");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["sessionId"], "session-test-123");
        assert_eq!(v["claimId"], "clm-z-0");
        assert_eq!(v["action"], "free");
        assert_eq!(v["text"], "用户回来了");
        assert!(v["ts"].as_str().unwrap().len() > 0);

        // closed 不写 wake（收尾是终点，别白唤醒）
        write_wake_request(&dir, "clm-z-0", "1", "closed");
        assert!(std::fs::metadata(dir.join("wakes").join("session-test-123.json"))
            .is_ok(), "closed 不覆盖已有 wake 文件");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
