use crate::config::{Config, Switches};
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

// 领任务台账的一条记录。
#[derive(Clone, Serialize, Deserialize)]
pub struct Claim {
    pub claim_id: String,
    pub task_id: String,
    pub task_name: String,
    pub project_id: String,
    pub stage_from: String,
    pub stage_to: String,
    pub agent: String,
    pub cwd: String,
    // 生命周期状态：
    //   running  - agent 正在干（进行中 + 列2），参与心跳超时判定
    //   waiting  - 已向用户提问，等回复（待处理 + 列2），**永不超时回收**
    //   done     - 已解决（列3）
    //   closed   - 关闭（列4）
    //   reverted - 会话异常断开，已被 sweep 回滚（待处理 + 列1）
    //   released - agent 主动放弃（待处理 + 列1）
    //   failed   - 记账后移动状态失败
    // 兼容旧台账：早期版本写的是 claimed，等价于 running。
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub note: String,
    // 最后一次心跳时间（rfc3339）。running 状态下超过阈值未更新即判定会话断开。
    #[serde(default)]
    pub heartbeat_at: String,
    // 认领时任务所在的看板列名（回滚时回到这里，通常是队列列）。
    #[serde(default)]
    pub tasklist_from: String,
    // 提问时间。tb reply 只读这个时间点之后的用户评论。
    #[serde(default)]
    pub ask_at: String,
    // 提的问题原文。留档，也用于在会话里回显。
    #[serde(default)]
    pub question: String,
}

impl Claim {
    // 是否还占着任务（running / waiting）。占着的任务不能被别人重复领。
    pub fn is_active(&self) -> bool {
        matches!(self.state.as_str(), "running" | "waiting" | "claimed")
    }

    // 是否正在执行（只有 running 参与心跳超时判定）。
    pub fn is_running(&self) -> bool {
        matches!(self.state.as_str(), "running" | "claimed")
    }

    // 心跳时间。旧台账没有这个字段，退回 updated_at / created_at。
    pub fn heartbeat(&self) -> &str {
        if !self.heartbeat_at.is_empty() {
            &self.heartbeat_at
        } else if !self.updated_at.is_empty() {
            &self.updated_at
        } else {
            &self.created_at
        }
    }
}

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}

// 台账独占锁的持有者。Drop 时释放；进程崩溃时由操作系统自动释放，
// 所以不会留下「僵尸锁文件」把后续所有命令卡死。
pub struct ClaimLock {
    file: std::fs::File,
}

impl Drop for ClaimLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

// 距今多少秒。解析失败返回 0（当作刚发生），宁可晚回收也不误判回滚。
pub fn age_secs(ts: &str) -> i64 {
    if ts.is_empty() {
        return 0;
    }
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(t) => {
            let then = t.with_timezone(&chrono::Utc);
            chrono::Utc::now().signed_duration_since(then).num_seconds()
        }
        Err(_) => 0,
    }
}

pub fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{:x}-{:x}", prefix, n, c)
}

impl Store {
    pub fn new() -> Store {
        Store {
            root: crate::config::data_dir(),
        }
    }

    pub fn ensure_dirs(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|e| format!("创建数据目录失败: {e}"))?;
        std::fs::create_dir_all(self.root.join("projects"))
            .map_err(|e| format!("创建项目目录失败: {e}"))?;
        Ok(())
    }

    // ---- config ----
    pub fn load_config(&self) -> Config {
        Config::load(&self.root)
    }

    pub fn save_config(&self, cfg: &Config) -> Result<(), String> {
        self.ensure_dirs()?;
        cfg.save(&self.root)
    }

    // ---- switches ----
    pub fn switches_path(&self) -> PathBuf {
        self.root.join("switches.toml")
    }

    pub fn load_switches(&self) -> Switches {
        let path = self.switches_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Switches::new(),
        }
    }

    pub fn save_switches(&self, sw: &Switches) -> Result<(), String> {
        self.ensure_dirs()?;
        let s = toml::to_string_pretty(sw).map_err(|e| e.to_string())?;
        std::fs::write(self.switches_path(), s).map_err(|e| e.to_string())
    }

    pub fn set_switch(&self, project_id: &str, skill: &str, on: bool) -> Result<(), String> {
        let mut sw = self.load_switches();
        sw.entry(project_id.to_string())
            .or_default()
            .insert(skill.to_string(), on);
        self.save_switches(&sw)
    }

    // ---- claims ledger ----
    pub fn claims_path(&self) -> PathBuf {
        self.root.join("claims.jsonl")
    }

    pub fn load_claims(&self) -> Vec<Claim> {
        let path = self.claims_path();
        let mut out = Vec::new();
        if let Ok(s) = std::fs::read_to_string(&path) {
            for line in s.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(c) = serde_json::from_str::<Claim>(line) {
                    out.push(c);
                }
            }
        }
        out
    }

    pub fn append_claim(&self, c: &Claim) -> Result<(), String> {
        self.ensure_dirs()?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.claims_path())
            .map_err(|e| e.to_string())?;
        let line = serde_json::to_string(c).map_err(|e| e.to_string())?;
        writeln!(f, "{}", line).map_err(|e| e.to_string())
    }

    pub fn save_claims(&self, claims: &[Claim]) -> Result<(), String> {
        self.ensure_dirs()?;
        let mut s = String::new();
        for c in claims {
            s.push_str(&serde_json::to_string(c).map_err(|e| e.to_string())?);
            s.push('\n');
        }
        std::fs::write(self.claims_path(), s).map_err(|e| e.to_string())
    }

    pub fn update_claim(
        &self,
        claim_id: &str,
        state: &str,
        note: &str,
    ) -> Result<(), String> {
        self.patch_claim(claim_id, |c| {
            c.state = state.to_string();
            if !note.is_empty() {
                c.note = note.to_string();
            }
        })
    }

    // 按闭包改一条 claim。所有「改状态 / 续命 / 记提问」都走这里，
    // 避免每次加字段都复制一遍 load-modify-save。
    pub fn patch_claim<F>(&self, claim_id: &str, mut f: F) -> Result<(), String>
    where
        F: FnMut(&mut Claim),
    {
        let mut claims = self.load_claims();
        let mut found = false;
        for c in claims.iter_mut() {
            if c.claim_id == claim_id {
                f(c);
                c.updated_at = now();
                found = true;
            }
        }
        if !found {
            return Err(format!("找不到 claim: {claim_id}"));
        }
        self.save_claims(&claims)
    }

    // 续命：只刷新心跳，不动状态。返回刷新后的 claim。
    pub fn beat_claim(&self, claim_id: &str) -> Result<Claim, String> {
        let ts = now();
        self.patch_claim(claim_id, |c| c.heartbeat_at = ts.clone())?;
        self.load_claims()
            .into_iter()
            .find(|c| c.claim_id == claim_id)
            .ok_or_else(|| format!("找不到 claim: {claim_id}"))
    }

    // 批量续命：把某个会话占着的所有 running 任务心跳刷一遍。
    //
    // 任何 tb 命令启动时都会顺带调它（见 cmds::run），这样长构建期间
    // 只要中途跑过一次 tb，就不会被当成僵尸会话回收。
    pub fn beat_agent(&self, agent: &str) -> usize {
        let ts = now();
        let mut claims = self.load_claims();
        let mut n = 0;
        for c in claims.iter_mut() {
            if c.agent == agent && c.is_running() {
                c.heartbeat_at = ts.clone();
                c.updated_at = ts.clone();
                n += 1;
            }
        }
        if n > 0 {
            let _ = self.save_claims(&claims);
        }
        n
    }

    // ---- 台账独占锁 ----
    //
    // 多个会话并行领任务时，「读台账 → 选任务 → 写台账」必须是原子的，
    // 否则两个进程会同时选中同一条任务。用操作系统文件锁实现，
    // 进程崩溃时锁由 OS 自动释放，不会留僵尸锁文件。
    pub fn lock_claims(&self, timeout_secs: u64) -> Result<ClaimLock, String> {
        self.ensure_dirs()?;
        let path = self.root.join("claims.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("打开台账锁失败: {e}"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(ClaimLock { file }),
                Err(_) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                            "另一个 tb 进程正占着台账（等了 {timeout_secs} 秒），先别并发领任务"
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }

    pub fn get_claim(&self, claim_id: &str) -> Option<Claim> {
        self.load_claims()
            .into_iter()
            .find(|c| c.claim_id == claim_id)
    }

    // 占着任务的活动 claim（running 或 waiting）。用于防重复领取。
    pub fn find_active_claim(&self, task_id: &str) -> Option<Claim> {
        self.load_claims()
            .into_iter()
            .find(|c| c.task_id == task_id && c.is_active())
    }

    // 当前所有活动 claim。sweep 和 status 用。
    pub fn active_claims(&self) -> Vec<Claim> {
        self.load_claims()
            .into_iter()
            .filter(|c| c.is_active())
            .collect()
    }

    // ---- 会话存活标记 ----
    //
    // tb serve（MCP 常驻进程）每 30 秒写一次自己的心跳文件，进程被杀就停写。
    // 用独立文件而不是改 claims.jsonl，是为了避开「心跳线程回写把状态改回去」
    // 的竞态：两个写者碰同一个文件，后写的会把先写的状态覆盖掉。
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn touch_session(&self, tag: &str) -> Result<(), String> {
        std::fs::create_dir_all(self.sessions_dir()).map_err(|e| e.to_string())?;
        std::fs::write(self.sessions_dir().join(format!("{tag}.hb")), now())
            .map_err(|e| e.to_string())
    }

    // 会话心跳距今多少秒。没有心跳文件返回 None（= 这个会话从没跑过 serve）。
    pub fn session_age(&self, tag: &str) -> Option<i64> {
        let p = self.sessions_dir().join(format!("{tag}.hb"));
        let s = std::fs::read_to_string(p).ok()?;
        Some(age_secs(s.trim()))
    }

    pub fn drop_session(&self, tag: &str) {
        let _ = std::fs::remove_file(self.sessions_dir().join(format!("{tag}.hb")));
    }

    // ---- shared project memory ----
    pub fn memory_dir(&self, project_id: &str) -> PathBuf {
        self.root.join("projects").join(project_id).join("memory")
    }

    pub fn write_memory(
        &self,
        project_id: &str,
        file: Option<&str>,
        content: &str,
    ) -> Result<PathBuf, String> {
        let dir = self.memory_dir(project_id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let name = file.unwrap_or("notes.md");
        let path = dir.join(name);
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
        Ok(path)
    }

    pub fn append_memory(&self, project_id: &str, content: &str) -> Result<PathBuf, String> {
        let dir = self.memory_dir(project_id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("notes.md");
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        writeln!(f, "\n## {stamp}\n{content}\n").map_err(|e| e.to_string())?;
        Ok(path)
    }

    pub fn read_memory(&self, project_id: &str) -> Vec<(String, String)> {
        let dir = self.memory_dir(project_id);
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                if p.is_file() {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if let Ok(s) = std::fs::read_to_string(&p) {
                        out.push((name, s));
                    }
                }
            }
        }
        out
    }

    // ---- mcp tools cache ----
    pub fn tools_cache_path(&self) -> PathBuf {
        self.root.join("tools-cache.json")
    }

    pub fn save_tools_cache(&self, tools: &[crate::tools::ToolInfo]) -> Result<(), String> {
        self.ensure_dirs()?;
        let s = serde_json::to_string_pretty(tools).map_err(|e| e.to_string())?;
        std::fs::write(self.tools_cache_path(), s).map_err(|e| e.to_string())
    }

    pub fn load_tools_cache(&self) -> Vec<crate::tools::ToolInfo> {
        let path = self.tools_cache_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<Vec<crate::tools::ToolInfo>>(&s).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    // 项目信息的小缓存，供 UI 秒开。
    pub fn projects_cache_path(&self) -> PathBuf {
        self.root.join("projects-cache.json")
    }

    pub fn save_projects_cache(&self, list: &[(String, String)]) -> Result<(), String> {
        self.ensure_dirs()?;
        let s = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
        std::fs::write(self.projects_cache_path(), s).map_err(|e| e.to_string())
    }

    pub fn load_projects_cache(&self) -> Vec<(String, String)> {
        let path = self.projects_cache_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<Vec<(String, String)>>(&s).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }
}
