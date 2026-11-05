use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// 五个固定任务阶段（= 工作流状态 / tfs）。每个 Teambition 项目都必须有。
// 注意：skill / mem 也走「工作流状态」维度，但**不是阶段**——它们是用途标记，
// 不参与生命周期（kind=unset），见 SKILL_STATE / MEMORY_STATE。
//
// 「待回复」是 agent 干到需要用户决策时用的独立状态：球在用户那，
// 但不属于「完成」（kind=unset），所以不影响完成率统计。
//
// 首尾两个名字刻意对齐 Teambition **新建项目自带的默认状态**（未完成 / 已完成）：
// 默认项目本来就有这两个，沿用就不用新建，也不会出现「未完成」和「待处理」两条并存的局面。
pub const STAGE_TODO: &str = "未完成";
pub const STAGE_DOING: &str = "进行中";
pub const STAGE_WAIT: &str = "待回复";
pub const STAGE_DONE: &str = "已解决";
pub const STAGE_CLOSED: &str = "已完成";
pub const ALL_STAGES: [&str; 5] = [
    STAGE_TODO,
    STAGE_DOING,
    STAGE_WAIT,
    STAGE_DONE,
    STAGE_CLOSED,
];

// ---- skill / 项目记忆：统一普通 Task + 两个工作流状态，内容存备注 ----
//
// 为什么不用任务类型(sfc)：sfc-create 能建类型，但自建类型（如「项目记忆」）的
// scenariofields 是空的，建任务/打标签会被服务端拒（10032 Scenariofieldconfig missed）。
// 为什么不用标签：`task.update-tags` 只能打**项目里已存在**的标签，工具集没有
// tag-create，新项目要先让人去网页建标签。
// 为什么不用自定义字段：rtf 建不了（MCP 网关只允许 text/number/date/dropDown/
// multipleChoice）且网页不渲染；text 受项目「自定义字段数量上限」限制
// （主项目 24 个已触顶，FeatureLimit 10034）。
//
// 备注(note) 是最干净的载体：内置字段、不占额度、网页任务详情直接可见可编辑。
// 实测（2026-10-27）：只传 `{"note": ...}` 能更新备注且**不动标题**，
// 5000 字完整读回、无截断（历史上「note 会落到标题」的结论是参数组合用错）。
//
// 于是：任务统一是普通 Task，靠**工作流状态**区分用途——
//   状态 mem   = 一条项目记忆（标题带节名，备注是正文）
//   状态 skill = 一条 skill（标题是名字，备注是正文）
// 两个状态由 tb 自动建（taskflow-update 的 tfsJson upsert），零手动。
pub const SKILL_STATE: &str = "skill";
pub const MEMORY_STATE: &str = "mem";
// 状态 setting = 这个项目的「编译 & 重载」参数（一条任务，标题固定「项目设置」，
// 备注是 TOML）。跟 skill/mem 一样由 tb 自动建，用户不用手动加。
pub const SETTING_STATE: &str = "setting";

// 记忆任务标题：`{节名} · {正文首行摘要}`，节名靠它被识别。
pub const MEMORY_TITLE_SEP: &str = " · ";
// 标题里摘要最多取多少字（太长看板一行显示不下，也不好扫）。
pub const MEMORY_TITLE_BRIEF: usize = 40;


// 项目记忆的分节。每条记忆任务属于一节，节名写在标题开头。
// 红线/决策是价值判断，只有用户能改；踩坑是客观事实，agent 可以写。
// 数组顺序 = 读取时的输出顺序（红线 → 决策 → 踩坑）。
pub const MEMORY_SECTION_RULES: &str = "红线";
pub const MEMORY_SECTION_DECISIONS: &str = "决策";
pub const MEMORY_SECTION_PITFALLS: &str = "踩坑";
pub const MEMORY_SECTIONS: [&str; 3] = [
    MEMORY_SECTION_RULES,
    MEMORY_SECTION_DECISIONS,
    MEMORY_SECTION_PITFALLS,
];

// 每节条数上限（= 该节的任务数）。超了只告警，不阻断、不自动删
// （自动删记忆比记忆臃肿危险得多）。
pub const MEMORY_SECTION_LIMIT: usize = 50;

// 单条记忆的字符上限（备注长度）。实测备注写 5000 字完整读回、无截断，
// 上限留 3000 字余量；超了直接报错让用户拆成多条——一条记忆本来就不该这么长。
pub const MEMORY_HARD_LIMIT_CHARS: usize = 8000;
pub const MEMORY_SOFT_LIMIT_CHARS: usize = 6000;

// 五个固定看板列（任务列表 / task list）。API 不能建、不能改名，只能在网页手动建，
// 所以这里按「列名」引用，列名对不上就跳过移列（只改状态），不阻断流程。
// 语义：列1=队列(没人接) / 列2=agent 正在干 / 列3=球在用户那 / 列4=已解决 / 列5=关闭。
//
// 列名统一用数字 1..5（用户拍板：不折腾中文列名）。tb init 会校验项目里有这 5 个
// 数字列，缺列就引导用户去 Teambition 网页手动建（网页 API 建不了列）。
pub const DEFAULT_TASKLIST_QUEUE: &str = "1";
pub const DEFAULT_TASKLIST_CLAIM: &str = "2";
pub const DEFAULT_TASKLIST_WAIT: &str = "3";
pub const DEFAULT_TASKLIST_DONE: &str = "4";
pub const DEFAULT_TASKLIST_CLOSED: &str = "5";

// 期望的 5 个数字列名（顺序即语义，跟上面五个 DEFAULT_ 对齐）。
pub const TASKLIST_WANT: [&str; 5] = [
    DEFAULT_TASKLIST_QUEUE,
    DEFAULT_TASKLIST_CLAIM,
    DEFAULT_TASKLIST_WAIT,
    DEFAULT_TASKLIST_DONE,
    DEFAULT_TASKLIST_CLOSED,
];

/// 项目现有列名是否覆盖了 5 个数字列。顺序不强制，只要有这 5 个名字即可。
/// tb init 用它判断要不要引导用户去网页手动补列。
pub fn tasklists_ready(names: &[&str]) -> bool {
    TASKLIST_WANT.iter().all(|w| names.contains(w))
}

/// Teambition 网页里的项目页前缀。任务链接是 `https://www.teambition.com/task/<id>`，
/// 项目按同一站点约定拼 `/project/<id>`。看板列网页 API 建不了，得把这条链接给人，
/// 让他自己进去手动新建 1~5。链接规则变了只改这一个常量。
pub const PROJECT_URL_PREFIX: &str = "https://www.teambition.com/project/";

// 会话心跳阈值（秒）。running 状态的 claim 超过这个时间没续命，
// 就判定「会话已断开」，回滚成 未完成 + 列1。waiting 状态永不超时。
pub const DEFAULT_HEARTBEAT_TIMEOUT_SECS: u64 = 300;

// tb wait 的默认参数：单轮 540 秒（Bash 工具上限 600 秒，留 60 秒余量让它主动返回，
// 避免被 auto-background 后 agent 拿不到结果），每 15 秒查一次。
pub const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 540;
pub const DEFAULT_WAIT_INTERVAL_SECS: u64 = 15;
// 邮件轮询默认比评论慢：IMAP 一次往返（登录 + SEARCH + FETCH）比读一条评论贵得多。
pub const DEFAULT_MAIL_POLL_SECS: u64 = 20;

// 下限。配置文件是手改的，GUI 也允许输入任意数字，写成 0 或 3 会把轮询变成忙等，
// 一路把 Teambition 接口和邮箱服务器打到限流。生效前统一夹一次。
pub const MIN_WAIT_TIMEOUT_SECS: u64 = 30;
pub const MIN_WAIT_INTERVAL_SECS: u64 = 5;
pub const MIN_MAIL_POLL_SECS: u64 = 5;


pub const DEFAULT_MCP_URL: &str = "https://open.teambition.com/api/mcp/v2";

// 邮件默认端口：465 = SMTP 隐式 TLS（最省事），993 = IMAP over TLS。
pub const DEFAULT_SMTP_PORT: u16 = 465;
pub const DEFAULT_IMAP_PORT: u16 = 993;

// 本地数据放在 exe 运行目录下的 data/。所有 agent 共享同一个目录。
//
// 安装包允许把 exe 装到任意位置（比如 D:\apps\tba），但历史数据多半在默认位置
// %USERPROFILE%\tba\data。同级没有 data\ 时回落到那里，免得换个目录就像重装一样
// 要重新登录、项目绑定也全没了。dev 构建（target\debug\data 已存在）不受影响。
pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let here = dir.join("data");
            if !here.exists() {
                if let Some(home) = std::env::var_os("USERPROFILE") {
                    let old = PathBuf::from(home).join("tba").join("data");
                    if old.is_dir() {
                        return old;
                    }
                }
            }
            return here;
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

    // ---- 文案里的工具路径 ----
    //
    // 留空 = 用当前 exe 的真实路径。填了 = 文案里一律写这个（比如指向一个短路径的
    // 目录联接 `D:\tb\tba.exe`）。文案每次开工都整段贴给 agent，路径要出现十几次，
    // 34 字符 × 14 次 ≈ 476 字符，占整份文案三分之一，所以值得单独配一个短的。
    #[serde(default)]
    pub tool_path: String,

    // ---- GitButler CLI 路径 ----
    //
    // 自动检测（环境变量 / 运行目录 / PATH / 已知安装目录）都没命中时，
    // 由用户在窗口里选一次 but.exe，存这里。**不扫盘**：扫盘又慢又容易挑到
    // 错误的副本，让用户指一次最准。环境变量仍然优先（临时覆盖/脚本用）。
    #[serde(default)]
    pub but_path: String,

    // 会话心跳阈值（秒）。running 超时未续命 -> 判定会话断开 -> 回滚到 未完成+列1。
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_secs: u64,

    // ---- 邮件通知（SMTP 发信 / IMAP 收信）----
    //
    // 关键事实：**SMTP 只能发，收不到回信**。要「客户直接回邮件也能继续」，
    // 必须再配 IMAP（本机轮询信箱，把回信写成任务评论）。
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_user: String,
    #[serde(default)]
    pub smtp_pass: String,
    // 发件人。留空则用 smtp_user。
    #[serde(default)]
    pub smtp_from: String,
    // 收件人（客户），多个用逗号或分号分隔。
    #[serde(default)]
    pub mail_to: String,
    // 到「待回复」时是否自动发邮件。
    #[serde(default = "default_true")]
    pub mail_on_ask: bool,
    // wait 发现「没 ask 过」时，是否自动补一次 ask（写备注 + 转待回复 + 发邮件）。
    //
    // 开：agent 做完忘了 ask，只要它跑了 wait，用户照样收得到邮件、回数字也有效。
    // 关：wait 只等外部收尾（用户手动改状态/拖列），不读评论、不发信。
    #[serde(default = "default_true")]
    pub wait_auto_ask: bool,
    #[serde(default)]
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    // ---- 等待与轮询（都在「待回复」这一段里生效）----
    //
    // 单轮最长等多久。到点返回退出码 2，agent 重跑同一条接着等。
    // 上限受宿主工具约束：Bash 工具 600 秒会转后台，所以默认 540 留 60 秒余量。
    #[serde(default = "default_wait_timeout")]
    pub wait_timeout_secs: u64,
    // 多久查一次任务评论。太小会让 Teambition API 压力大，太大回复延迟高。
    #[serde(default = "default_wait_interval")]
    pub wait_interval_secs: u64,
    // 多久查一次邮箱（IMAP 一次往返比读评论贵，所以默认比评论间隔长）。
    #[serde(default = "default_mail_poll")]
    pub mail_poll_secs: u64,

    // ---- 离线模式 ----
    //
    // 不开 Teambition 也能用：项目列表、项目能力（skill）、开工文案全部走本地缓存，
    // 通用能力（data/caps.toml）和编译 & 重载设置本来就是本地的，照旧。
    // 需要云端才成立的操作（领任务、done、ask/wait、刷新项目）在离线时直接说明不可用。
    //
    // 为什么要有：项目可能在内网/断网环境，或者不想把项目结构同步到云端；
    // 但「生成一份开工文案给 agent」这件事本身不需要云端。
    #[serde(default)]
    pub offline: bool,
}

// 默认值。字段一多就不能靠 derive：每个字段的默认值不一样，
// 而且有几个必须走函数（列名、端口），所以手写。
impl Default for Config {
    fn default() -> Config {
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
            tool_path: String::new(),
            but_path: String::new(),
            heartbeat_timeout_secs: default_heartbeat_timeout(),
            smtp_host: String::new(),
            smtp_port: default_smtp_port(),
            smtp_user: String::new(),
            smtp_pass: String::new(),
            smtp_from: String::new(),
            mail_to: String::new(),
            mail_on_ask: true,
            wait_auto_ask: true,
            imap_host: String::new(),
            imap_port: default_imap_port(),
            wait_timeout_secs: default_wait_timeout(),
            wait_interval_secs: default_wait_interval(),
            mail_poll_secs: default_mail_poll(),
            offline: false,
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

    /// 这次运行是不是离线模式。
    ///
    /// 环境变量优先（`TBA_OFFLINE=1` 强开、`=0` 强关），没设就看 config.toml——
    /// 这样脚本/排查能临时切换，不用改配置文件。
    pub fn offline(&self) -> bool {
        match std::env::var("TBA_OFFLINE")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("1") | Some("true") | Some("on") | Some("yes") => true,
            Some("0") | Some("false") | Some("off") | Some("no") => false,
            _ => self.offline,
        }
    }
}

// 本地 skill 开关。project_id -> (skill 名 -> 开关)。只存本地。
pub type Switches = HashMap<String, HashMap<String, bool>>;

// tb 共享记忆的项目级开关。存 projects/<id>/tb-memory.toml。
//
// 记忆正文不放这里：正文是 Teambition 上状态为 mem 的任务备注，云端、跨设备。
// 这里只存「这个项目要不要用 tb 的记忆」。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TbMemory {
    #[serde(default)]
    pub enabled: bool,
}

impl TbMemory {
    // 记忆任务的状态名。固定值，跟 enabled 无关——开关关掉时任务还在。
    pub fn state(&self) -> &'static str {
        MEMORY_STATE
    }
}

fn default_true() -> bool {
    true
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

fn default_smtp_port() -> u16 {
    DEFAULT_SMTP_PORT
}
fn default_imap_port() -> u16 {
    DEFAULT_IMAP_PORT
}
fn default_wait_timeout() -> u64 {
    DEFAULT_WAIT_TIMEOUT_SECS
}
fn default_wait_interval() -> u64 {
    DEFAULT_WAIT_INTERVAL_SECS
}
fn default_mail_poll() -> u64 {
    DEFAULT_MAIL_POLL_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasklists_ready_accepts_1_to_5() {
        assert!(tasklists_ready(&["1", "2", "3", "4", "5"]));
    }

    #[test]
    fn tasklists_ready_tolerates_extra_and_order() {
        // 顺序不强制、允许多余列：只要有这 5 个数字即可。
        assert!(tasklists_ready(&["5", "4", "3", "2", "1", "归档"]));
    }

    #[test]
    fn tasklists_ready_rejects_missing_column() {
        // 旧项目只有 4 列，缺「5」→ 不齐，引导补列。
        assert!(!tasklists_ready(&["1", "2", "3", "4"]));
    }

    #[test]
    fn tasklists_ready_rejects_chinese_names() {
        // 用户拍板用数字；中文列名视为不齐，引导改成数字。
        assert!(!tasklists_ready(&["待处理", "进行中", "待回复", "已解决", "关闭"]));
    }

    // 离线开关要能落盘、读回来。默认在线（不能把老配置读成离线，
    // 那会让人以为「tba 突然不干活了」）。
    #[test]
    fn offline_flag_roundtrips_in_toml() {
        let mut c = Config::default();
        assert!(!c.offline, "默认必须是在线");
        c.offline = true;
        let s = toml::to_string_pretty(&c).unwrap();
        assert!(s.contains("offline = true"), "{s}");
        let back: Config = toml::from_str(&s).unwrap();
        assert!(back.offline, "写进去要能读回来");
    }
}
