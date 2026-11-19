// 常驻监听期间收到的用户回复，落盘留档。
//
// 为什么需要这个文件：会话唤醒靠「后台 shell 跑完」这条通知，而 agent 在
// 「执行中」干活时，用户可能已经先在评论/邮件里回了一句。监听进程先把原文
// 写进 events.jsonl 再退出，哪怕会话还没被唤起、监听进程后来被重启，这句话
// 也不会丢。`tba events` 随时能翻出来看。
//
// 只追加、不改写：多个监听进程同时写也不会互相覆盖。
//
// 分流：落盘时带 action/len，默认卡只给摘要，原文按需 `tba events --full`。
// 原因（用户原话）：主代理上下文不能太长，我怕他主代理先抽风或者出问题。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: String,
    pub claim_id: String,
    pub source: String,
    pub text: String,
    // 以下三个旧 jsonl 行没有；load 用 serde(default) 兼容。
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub len: usize,
    #[serde(default)]
    pub handled: bool,
}

/// 摘要：首行、去空白，最多 limit 个字符（按 char 计，中文不切半）。
pub fn summary(text: &str, limit: usize) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = first.chars().take(limit).collect();
    if first.chars().count() > limit {
        out.push('…');
    }
    out
}

pub fn path(root: &Path) -> PathBuf {
    root.join("events.jsonl")
}

pub fn append(root: &Path, claim_id: &str, source: &str, text: &str, action: &str) {
    let e = Event {
        ts: chrono::Local::now().to_rfc3339(),
        claim_id: claim_id.to_string(),
        source: source.to_string(),
        text: text.to_string(),
        action: action.to_string(),
        len: text.chars().count(),
        handled: false,
    };
    let Ok(line) = serde_json::to_string(&e) else {
        return;
    };
    use std::io::Write;
    if let Some(dir) = path(root).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path(root))
    {
        let _ = writeln!(f, "{line}");
    }
}

pub fn load(root: &Path) -> Vec<Event> {
    std::fs::read_to_string(path(root))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Event>(l.trim()).ok())
        .collect()
}

/// 最近 n 条。claim_id 给了就只筛这个任务。
pub fn recent(root: &Path, n: usize, claim_id: Option<&str>) -> Vec<Event> {
    let mut v: Vec<Event> = load(root)
        .into_iter()
        .filter(|e| claim_id.map_or(true, |c| e.claim_id == c))
        .collect();
    let start = v.len().saturating_sub(n.max(1));
    v.drain(..start);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tba-events-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn append_and_recent_keep_the_original_text() {
        let dir = tmp("basic");
        let text = "邮件回复后反应很慢？能不能缩短？反应快一点";
        append(&dir, "clm-a", "mail", text, "free");
        append(&dir, "clm-b", "comment", "2", "summarize");
        let all = load(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].text, text);
        assert_eq!(all[0].action, "free");
        assert_eq!(all[0].len, text.chars().count());
        assert!(!all[0].handled);
        assert_eq!(all[1].action, "summarize");
        let only_a = recent(&dir, 10, Some("clm-a"));
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].text, text);
        assert_eq!(only_a[0].claim_id, "clm-a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_filters_by_claim_and_caps_count() {
        let dir = tmp("filter");
        for i in 0..5 {
            append(&dir, "clm-a", "mail", &format!("a{i}"), "free");
        }
        append(&dir, "clm-b", "comment", "b", "closed");
        let two = recent(&dir, 2, Some("clm-a"));
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].text, "a3");
        assert_eq!(two[1].text, "a4");
        let all = recent(&dir, 10, None);
        assert_eq!(all.len(), 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_lines_without_new_fields_load_with_defaults() {
        let dir = tmp("legacy");
        std::fs::write(
            path(&dir),
            r#"{"ts":"2026-01-01T00:00:00+08:00","claim_id":"clm-old","source":"mail","text":"离线模式"}"#,
        )
        .unwrap();
        let all = load(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "离线模式");
        assert_eq!(all[0].action, "");
        assert_eq!(all[0].len, 0);
        assert!(!all[0].handled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_cuts_first_line_and_chars() {
        assert_eq!(summary("短句", 40), "短句");
        assert_eq!(summary("第一行\n第二行", 40), "第一行");
        let long = "一二三四五六七八九十".repeat(6);
        let s = summary(&long, 40);
        assert_eq!(s.chars().count(), 41); // 40 字 + 省略号
        assert!(s.ends_with('…'));
        assert_eq!(summary("   \n内容  ", 40), "内容");
    }
}
