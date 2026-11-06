// 常驻监听期间收到的用户回复，落盘留档。
//
// 为什么需要这个文件：会话唤醒靠「后台 shell 跑完」这条通知，而 agent 在
// 「执行中」干活时，用户可能已经先在评论/邮件里回了一句。监听进程先把原文
// 写进 events.jsonl 再退出，哪怕会话还没被唤起、监听进程后来被重启，这句话
// 也不会丢。`tba events` 随时能翻出来看。
//
// 只追加、不改写：多个监听进程同时写也不会互相覆盖。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: String,
    pub claim_id: String,
    pub source: String,
    pub text: String,
}

pub fn path(root: &Path) -> PathBuf {
    root.join("events.jsonl")
}

pub fn append(root: &Path, claim_id: &str, source: &str, text: &str) {
    let e = Event {
        ts: chrono::Local::now().to_rfc3339(),
        claim_id: claim_id.to_string(),
        source: source.to_string(),
        text: text.to_string(),
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

    #[test]
    fn append_and_recent_keep_the_original_text() {
        let dir = std::env::temp_dir().join(format!("tba-events-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = "邮件回复后反应很慢？能不能缩短？反应快一点";
        append(&dir, "clm-a", "mail", text);
        append(&dir, "clm-b", "comment", "2");
        let all = load(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].text, text);
        let only_a = recent(&dir, 10, Some("clm-a"));
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].text, text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
