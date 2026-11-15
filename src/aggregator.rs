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
    beat, finish, parse_reply, resume, write_wake_request, ReplyAction,
};
use crate::store::Store;
use crate::tools::CommentInfo;
use std::collections::HashMap;
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
                        for c in &active {
                            let marker = format!("[TB {}]", c.claim_id);
                            if !marker_hit(&subject, &marker) && !marker_hit(&body, &marker) {
                                continue;
                            }
                            let text = crate::mail::clean_reply(&body, &marker);
                            events::append(&store.root, &c.claim_id, "mail", &text);
                            let wake = format!("（邮件回复）\n\n{text}");
                            write_wake_request(&store.root, &c.claim_id, &wake);
                            dispatch_reply(&store, &c.claim_id, &wake, "mail");
                            break; // 一封邮件只属于一个 claim
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
                if let Some(last) = fresh.last() {
                    let text = last.text.clone();
                    events::append(&store.root, &c.claim_id, "comment", &text);
                    write_wake_request(&store.root, &c.claim_id, &text);
                    dispatch_reply(&store, &c.claim_id, &text, "comment");
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

// 分发一条回复：parse_reply 判动作，回 1 才 finish；其余 resume/noted 不改终态。
// 与旧 wait() 的语义保持一致，只是不再有 emit/前台等待者——唤醒由 write_wake_request 负责。
fn dispatch_reply(store: &Store, claim_id: &str, text: &str, _source: &str) {
    let (action, _body) = parse_reply(text);
    let claim = match store.get_claim(claim_id) {
        Some(c) => c,
        None => return,
    };
    let has_ask = !claim.ask_at.is_empty();
    if !has_ask {
        // 执行中态：不 resume、不改状态，原话已在 events/wakes 里，交给 agent 决定。
        return;
    }
    // 待回复态：先 resume 推回进行中。
    match resume(store, claim_id) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("… 聚合回复 {claim_id} 时 resume 失败: {e}");
            return;
        }
    }
    if action == ReplyAction::Closed {
        if let Err(e) = finish(store, claim_id, STAGE_DONE) {
            eprintln!("… 聚合收尾 {claim_id} 失败: {e}");
        }
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
}
