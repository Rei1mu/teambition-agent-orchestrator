use clap::{Parser, Subcommand};

mod capability;
mod cmds;
mod config;
mod mcp_server;
mod oauth;
mod ops;
mod store;
mod tb;
mod tools;
mod ui;

#[derive(Parser)]
#[command(
    name = "tb",
    version,
    about = "TBHub - Teambition 共享 Skill / 任务中枢 (CLI + MCP + 窗口)"
)]
pub struct Cli {
    // 子命令可选：不带参数直接跑 = 打开 GUI（双击 tb / 直接 tb 即窗口）。
    // agent 调用一律用显式子命令走 CLI。
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// 登录。tb login --token <令牌>，或配置 OAuth 后 tb login
    Login {
        #[arg(long)]
        token: Option<String>,
    },
    /// 退出登录
    Logout,
    /// 查看/切换本地配置。切换服务地址不会动 token
    Config {
        /// 显示当前配置(token 打码)
        #[arg(long)]
        show: bool,
        /// 切换 MCP 服务地址。已登录的 token 会保留
        #[arg(long)]
        url: Option<String>,
    },
    /// 列出企业下所有项目
    Projects,
    /// 确保项目有 5 个阶段: 待处理/进行中/待回复/已解决/关闭。缺的自动补
    Init {
        #[arg(long)]
        project: String,
    },
    /// 列出项目的所有 skill 及本地开关
    Skills {
        #[arg(long)]
        project: String,
    },
    /// 打印单个 skill 的完整内容。文案里只列 skill 名字，agent 用到哪个读哪个（省 token）
    Skill {
        /// skill 名字（和 tb skills 列出的一致）
        name: String,
        #[arg(long)]
        project: String,
    },
    /// 把本地文件推成一条 skill（任务类型=skill）。同名则更新内容
    SkillPush {
        #[arg(long)]
        project: String,
        #[arg(long)]
        file: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// 打开/关闭本地 skill 开关。开关只存本地
    Switch {
        #[arg(long)]
        project: String,
        #[arg(long)]
        skill: String,
        #[arg(long)]
        on: bool,
        #[arg(long)]
        off: bool,
    },
    /// 按阶段列出任务
    Tasks {
        #[arg(long)]
        project: String,
        #[arg(long)]
        stage: Option<String>,
    },
    /// 查一条任务现在到底在哪：工作流状态 + 看板列 + 是否完成
    Show {
        /// 任务链接 https://www.teambition.com/task/<id> 或裸任务 id
        target: String,
    },
    /// 领一条「待处理」任务。默认取创建时间最早的一条。自动转「进行中」
    Claim {
        #[arg(long)]
        project: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// 接单：给任务链接或 id，改成「进行中」并挪到看板列「进行中」
    Take {
        /// 任务链接 https://www.teambition.com/task/<id> 或裸任务 id
        target: String,
        /// 记录是谁接的（多 agent 时用来区分）
        #[arg(long)]
        agent: Option<String>,
    },
    /// 开工：没有现成任务时新建一条，直接进入「进行中」+ 看板列「进行中」
    Start {
        #[arg(long)]
        project: String,
        /// 任务标题。留空则从 --file 的 `# 标题` 行或文件名推断
        #[arg(long)]
        title: Option<String>,
        /// 任务正文（短文本）
        #[arg(long)]
        note: Option<String>,
        /// 任务正文文件（markdown）。和 --note 二选一，同时给以 --note 为准
        #[arg(long)]
        file: Option<String>,
        /// 记录是谁开工的
        #[arg(long)]
        agent: Option<String>,
    },
    /// 提问：改成「待回复」+ 看板列「待回复」，把问题写成任务评论等你回复
    Ask {
        claim_id: String,
        /// 要问用户的话
        #[arg(long, short = 'q')]
        question: String,
    },
    /// 继续（只看一眼）：读回你在 Teambition 任务里的评论，状态改回「进行中」
    Reply { claim_id: String },
    /// 登记当前会话标识。同一目录要跑多个会话时用 --name 区分
    Hello {
        /// 会话标识，缺省自动生成（如 s-3f2a1c）
        #[arg(long)]
        name: Option<String>,
    },
    /// 列出所有「待回复」的任务：谁在等、等了多久、问了什么
    Inbox {
        /// 只看某个会话占的任务（默认全部）
        #[arg(long)]
        agent: Option<String>,
    },
    /// 同步等回复：阻塞轮询任务评论，你在 Teambition 回一句就返回正文并继续
    Wait {
        claim_id: String,
        /// 单轮最长等待秒数（默认 540，留余量给工具 600 秒上限）
        #[arg(long)]
        timeout: Option<u64>,
        /// 轮询间隔秒数（默认 15）
        #[arg(long)]
        interval: Option<u64>,
    },
    /// 续命：刷新心跳。长构建/长测试前后调用，避免被判定会话断开
    Beat { claim_id: String },
    /// 退回队列：状态改「待处理」并挪回看板列「待处理」（主动放弃）
    Release {
        claim_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// 回收心跳超时的会话：把它们的任务退回队列
    Sweep {
        /// 无视超时，强制回收所有进行中的 claim
        #[arg(long)]
        force: bool,
    },
    /// 把已领任务转为「已解决」
    Done { claim_id: String },
    /// 把已领任务转为「关闭」
    Close { claim_id: String },
    /// 查看本地领取台账
    Status,
    /// 生成给 agent 的调用文案(英文速查表 + 已启用 skill 名字列表，不含 skill 全文)
    Prompt {
        #[arg(long)]
        project: String,
        /// 本次任务描述。给了就写进文案，agent 会新建任务直接开工；不给就让 agent 去队列领
        #[arg(long)]
        task: Option<String>,
        /// 本次任务正文文件(markdown)。和 --task 同时给时，正文以文件为准
        #[arg(long)]
        task_file: Option<String>,
    },
    /// 读写项目共享记忆
    Memory {
        #[arg(long)]
        project: String,
        #[arg(long)]
        write: Option<String>,
        #[arg(long)]
        file: Option<String>,
    },
    /// 查看 MCP 工具列表和角色识别结果
    Tools {
        #[arg(long)]
        refresh: bool,
        /// 输出每个工具的完整 inputSchema(JSON)。排查参数问题时用
        #[arg(long)]
        json: bool,
    },
    /// 以 MCP server (stdio) 运行。给 agent 挂载用
    Serve,
    /// 打开 egui 窗口
    Ui,
    /// 通用能力：重载。编译 -> 杀旧 -> 复制校验 -> 启动
    Reload {
        /// build | restart | kill | status
        #[arg(default_value = "build")]
        action: String,
        /// 目标项目根目录。缺省按 exe 位置推断
        #[arg(long)]
        root: Option<String>,
        /// exe 基名。缺省读 Cargo.toml
        #[arg(long)]
        name: Option<String>,
        /// 运行副本路径。缺省 <root>/target/release/<name>_test.exe
        #[arg(long)]
        live: Option<String>,
        /// 启动副本时传的参数。重载 tb 自己时缺省 ui
        #[arg(long)]
        args: Option<String>,
        /// 模式: rename=改名直跑(缺省) / copy=副本运行
        #[arg(long)]
        mode: Option<String>,
        /// 杀旧/启动走 UAC 提权
        #[arg(long)]
        elevate: bool,
    },
}

fn main() {
    if let Err(e) = crate::cmds::run() {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}
