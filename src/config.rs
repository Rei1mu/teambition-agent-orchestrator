use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// 五个固定任务阶段（= 工作流状态 / tfs）。每个 Teambition 项目都必须有。
// 注意：技能(skill) 不再是阶段，而是「任务类型」(sfc)，见 TASK_TYPE_SKILL。
//
// 「待回复」是 agent 干到需要用户决策时用的独立状态：球在用户那，
// 但不属于「完成」（kind=unset），所以不影响完成率统计。
pub const STAGE_TODO: &str = "待处理";
pub const STAGE_DOING: &str = "进行中";
pub const STAGE_WAIT: &str = "待回复";
pub const STAGE_DONE: &str = "已解决";
pub const STAGE_CLOSED: &str = "关闭";
pub const ALL_STAGES: [&str; 5] = [
    STAGE_TODO,
    STAGE_DOING,
    STAGE_WAIT,
    STAGE_DONE,
    STAGE_CLOSED,
];

// 任务类型（sfc / 任务类型维度）：普通任务用默认 Task 类型，技能用 skill 类型。
pub const TASK_TYPE_SKILL: &str = "skill";

// 五个固定看板列（任务列表 / task list）。API 不能建、不能改名，只能在网页手动建，
// 所以这里按「列名」引用，列名对不上就跳过移列（只改状态），不阻断流程。
// 语义：列1=队列(没人接) / 列2=agent 正在干 / 列3=球在用户那 / 列4=已解决 / 列5=关闭。
//
// 默认用中文列名而不是数字：列名不存在时只会「跳过移列」，
// 而用数字（1..4）在只有 4 列的旧项目上会把「已解决」错移进「关闭」列。
pub const DEFAULT_TASKLIST_QUEUE: &str = "待处理";
pub const DEFAULT_TASKLIST_CLAIM: &str = "进行中";
pub const DEFAULT_TASKLIST_WAIT: &str = "待回复";
pub const DEFAULT_TASKLIST_DONE: &str = "已解决";
pub const DEFAULT_TASKLIST_CLOSED: &str = "关闭";

// 会话心跳阈值（秒）。running 状态的 claim 超过这个时间没续命，
// 就判定「会话已断开」，回滚成 待处理 + 列1。waiting 状态永不超时。
pub const DEFAULT_HEARTBEAT_TIMEOUT_SECS: u64 = 300;

// tb wait 的默认参数：单轮 540 秒（Bash 工具上限 600 秒，留 60 秒余量让它主动返回，
// 避免被 auto-background 后 agent 拿不到结果），每 15 秒查一次。
pub const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 540;
pub const DEFAULT_WAIT_INTERVAL_SECS: u64 = 15;

pub const DEFAULT_MCP_URL: &str = "https://open.teambition.com/api/mcp/v2";

// 本地数据放在 exe 运行目录下的 data/。所有 agent 共享同一个目录。
pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("data");
        }
    }
    PathBuf::from("data")
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_mcp_url")]
    pub mcp_url: String,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub enterprise_id: Option<String>,
    // OAuth 可选配置。留空则只能用 tb login --token。
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub authorize_url: Option<String>,
    #[serde(default)]
    pub token_url: Option<String>,
    // 工具名覆盖。键是角色名，值是 MCP 工具名。自动识别不准时用手工指定。
    #[serde(default)]
    pub tool_overrides: HashMap<String, String>,

    // ---- 看板列名（任务列表）----
    // 列名对不上时跳过移列，只改状态，不阻断流程。
    #[serde(default = "default_tasklist_queue")]
    pub tasklist_queue: String,
    #[serde(default = "default_tasklist_claim")]
    pub tasklist_claim: String,
    #[serde(default = "default_tasklist_wait")]
    pub tasklist_wait: String,
    #[serde(default = "default_tasklist_done")]
    pub tasklist_done: String,
    #[serde(default = "default_tasklist_closed")]
    pub tasklist_closed: String,

    // 会话心跳阈值（秒）。running 超时未续命 -> 判定会话断开 -> 回滚到 待处理+列1。
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_secs: u64,
}

fn default_tasklist_queue() -> String {
    DEFAULT_TASKLIST_QUEUE.to_string()
}
fn default_tasklist_claim() -> String {
    DEFAULT_TASKLIST_CLAIM.to_string()
}
fn default_tasklist_wait() -> String {
    DEFAULT_TASKLIST_WAIT.to_string()
}
fn default_tasklist_done() -> String {
    DEFAULT_TASKLIST_DONE.to_string()
}
fn default_tasklist_closed() -> String {
    DEFAULT_TASKLIST_CLOSED.to_string()
}
fn default_heartbeat_timeout() -> u64 {
    DEFAULT_HEARTBEAT_TIMEOUT_SECS
}

fn default_mcp_url() -> String {
    DEFAULT_MCP_URL.to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mcp_url: default_mcp_url(),
            access_token: None,
            enterprise_id: None,
            client_id: None,
            client_secret: None,
            authorize_url: None,
            token_url: None,
            tool_overrides: HashMap::new(),
            tasklist_queue: default_tasklist_queue(),
            tasklist_claim: default_tasklist_claim(),
            tasklist_wait: default_tasklist_wait(),
            tasklist_done: default_tasklist_done(),
            tasklist_closed: default_tasklist_closed(),
            heartbeat_timeout_secs: default_heartbeat_timeout(),
        }
    }
}

impl Config {
    pub fn load(root: &Path) -> Config {
        let path = root.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let s = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(root.join("config.toml"), s).map_err(|e| e.to_string())
    }

    pub fn token(&self) -> Option<&str> {
        self.access_token.as_deref().filter(|t| !t.is_empty())
    }
}

// 本地 skill 开关。project_id -> (skill 名 -> 开关)。只存本地。
pub type Switches = HashMap<String, HashMap<String, bool>>;
