use clap::{Parser, Subcommand};

mod capability;
mod caps;
mod cmds;
mod config;
mod create;
mod git;
mod gitbutler;
mod mail;
mod mcp_server;
mod naming;
mod oauth;
mod ops;
mod proc;
mod setting;
mod shot;
mod store;
mod tb;
mod toolchain;
mod tools;
mod ui;

#[derive(Parser)]
#[command(
    name = "tba",
    version,
    about = "TBA - Teambition Agent (CLI + MCP + 窗口)"
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
        /// 文案里的工具路径（可指向短路径的目录联接，省 token）。传空串清除
        #[arg(long)]
        tool: Option<String>,
    },
    /// GitButler CLI 绑定：status 看状态 / install 装到运行目录 / path 打印路径。
    /// 优先级高于 Teambition：缺它 TBHub 不干活
    #[command(name = "gitbutler")]
    GitButler {
        /// status | install | path | set | clear
        #[arg(default_value = "status")]
        action: String,
        /// set 用：but.exe 路径，或它所在的目录
        #[arg(long)]
        path: Option<String>,
    },
    /// 构建工具链（MSVC + Windows SDK）：status | set | guess | clear。
    /// 只认 MSVC，不做 gnu 兜底；标准位置找不到就用 set 指定一次
    #[command(name = "toolchain", visible_alias = "tc")]
    Toolchain {
        /// status | set | guess | clear
        #[arg(default_value = "status")]
        action: String,
        /// MSVC 的 bin 目录（link.exe 所在）：VC/Tools/MSVC/<ver>/bin/Hostx64/x64
        #[arg(long = "msvc-bin")]
        msvc_bin: Option<String>,
        /// MSVC 的 lib 目录（libcmt.lib 所在）：VC/Tools/MSVC/<ver>/lib/x64
        #[arg(long = "msvc-lib")]
        msvc_lib: Option<String>,
        /// Windows SDK 的 lib 目录（kernel32.lib 所在）。多个用 ; 分隔
        #[arg(long = "sdk-lib")]
        sdk_lib: Option<String>,
        /// 额外的库目录（可选）
        #[arg(long = "extra-lib")]
        extra_lib: Option<String>,
    },
    /// 列出企业下所有项目
    Projects,
    /// 确保项目有 7 个工作流状态: 未完成/进行中/待回复/已解决/已完成 + skill/mem。缺的自动补
    Init {
        #[arg(long)]
        project: String,
    },
    /// 给当前目录接一个 Teambition 项目：有就复用，没有就新建，并补齐阶段/记忆状态。
    /// 幂等，可反复跑。成功后把 project id 记到 .tba/project
    EnsureProject {
        /// 项目名。给了但项目不存在就新建
        #[arg(long)]
        name: Option<String>,
        /// 已有项目 id（跳过按名字查找）
        #[arg(long)]
        project: Option<String>,
        /// 只扫目录列出候选项目名（含来源、是否已有同名），不创建、不写绑定。
        /// 给人拍板用：先看候选，再决定 --name 填什么
        #[arg(long)]
        dry_run: bool,
        /// 确实要用当前目录名当项目名（不给 --name / --project 时必须显式加这个）
        #[arg(long)]
        yes: bool,
    },
    /// 接入项目：扫目录列候选名 -> 你拍板 -> 建/复用项目 -> 补 7 状态 ->
    /// 写 .tba/project -> 补 AGENTS.md 入口 -> 打印开工文案。一条命令跑完初始化文案
    Create {
        /// 项目名。给了就直接用，跳过交互拍板
        #[arg(long)]
        name: Option<String>,
        /// 目标目录。缺省当前目录；窗口里可以选任意目录
        #[arg(long)]
        dir: Option<String>,
        /// 已有项目 id：同名多个时指定复用哪个
        #[arg(long)]
        project: Option<String>,
        /// 只列候选，不建项目、不写文件
        #[arg(long)]
        dry: bool,
        /// 开工文案语言：zh / en
        #[arg(long, default_value = "zh")]
        lang: String,
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
    /// 把内置 skill 模板推到项目里（shot / git-main / release / gui-notes）。同名覆盖
    SkillInit {
        #[arg(long)]
        project: String,
        /// 只推某一个。不给就推全部
        #[arg(long)]
        name: Option<String>,
    },
    /// 把项目 skill / mem 拉成本地两个 md：data/projects/<id>/skillmem/{skill,mem}.md
    SkillMem {
        #[arg(long)]
        project: String,
        /// 导出后直接打开文件夹
        #[arg(long)]
        open: bool,
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
    /// 一句话开工：按当前目录绑定的项目领一条「未完成」任务，打印正文和流程。
    /// 退出码 3=目录未绑定项目 / 4=项目查不到 / 0=已领到或队列空(EMPTY)
    #[command(visible_aliases = ["g", "r", "run"])]
    Go {
        /// 项目 id。省略则按 .tba/project 绑定解析
        #[arg(long)]
        project: Option<String>,
        /// 文案语言：zh=中文(准本) / en=英文(省 token)
        #[arg(long, default_value = "zh")]
        lang: String,
        /// 不自动往当前目录补 AGENTS.md 入口
        #[arg(long)]
        no_entry: bool,
        /// 直连模式：不领任务、不碰 Teambition，直接在当前对话框里交互
        #[arg(long)]
        off: bool,
        /// 这次要做的事。直连模式下就是「本次任务」，多个词直接写，不用引号。
        /// 例：tba go -off 把侧栏改窄一点
        #[arg(num_args = 0.., value_name = "TASK")]
        task: Vec<String>,
    },
    /// 领一条「未完成」任务。默认取创建时间最早的一条。自动转「进行中」
    Claim {
        /// 项目 id。省略则按 .tba/project 绑定解析
        #[arg(long)]
        project: Option<String>,
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
        /// 项目 id。省略则按 .tba/project 绑定解析
        #[arg(long)]
        project: Option<String>,
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
    /// 提问：改成「待回复」+ 看板列「待回复」，问题写进任务备注，发邮件，
    /// 然后就地阻塞等你回数字（会话不会断）。加 --no-wait 只问不等。
    Ask {
        /// claim id（--off 时可以省略）
        claim_id: Option<String>,
        /// 要问用户的话
        #[arg(long, short = 'q')]
        question: Option<String>,
        /// 用标准的「做完了，请确认」三选项（0 收尾 / 1 先写记忆再收尾 / 2 还有问题）
        #[arg(long)]
        done: bool,
        /// 问完立刻返回，不阻塞等待（默认是就地等到用户回数字）
        #[arg(long)]
        no_wait: bool,
        /// 直连模式：不写任务、不发邮件、不阻塞，只把问题打到 stdout，
        /// 由 agent 在当前对话框里问用户
        #[arg(long)]
        off: bool,
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
        /// claim id（--off 时可以省略）
        claim_id: Option<String>,
        /// 单轮最长等待秒数（默认 540，留余量给工具 600 秒上限）
        #[arg(long)]
        timeout: Option<u64>,
        /// 轮询间隔秒数（默认 15）
        #[arg(long)]
        interval: Option<u64>,
        /// 直连模式：不轮询 Teambition，直接返回（直连模式下本来就不需要等）
        #[arg(long)]
        off: bool,
    },
    /// 续命：刷新心跳。长构建/长测试前后调用，避免被判定会话断开
    Beat { claim_id: String },
    /// 把会话记录 / 结论追加到任务备注（往后追加，不清空）。评论留给用户，agent 不写
    Note {
        claim_id: String,
        /// 要追加的文字
        #[arg(long)]
        text: Option<String>,
        /// 从文件读(和 --text 二选一，同时给以 --file 为准)
        #[arg(long)]
        file: Option<String>,
    },
    /// 邮件：检查配置 / 发一封测试信 / 收一次回信
    Mail {
        /// 动作: status(只看配置) | test(发测试信) | check(收一次回信) | resend(重发某条任务的通知)
        #[arg(default_value = "status")]
        action: String,
        /// resend 时指定 claim_id
        arg: Option<String>,
    },
    /// 调试：直接调任意 Teambition MCP 工具，打印原始 JSON
    Raw {
        /// 工具名，如 teambition.project.file-list
        #[arg(default_value = "")]
        tool: String,
        /// JSON 参数，如 '{"projectId":"xxx"}'
        #[arg(long)]
        args: Option<String>,
        /// 列出全部可用工具名
        #[arg(long)]
        list: bool,
    },
    /// 退回队列：状态改「未完成」并挪回看板列「未完成」（主动放弃）
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
    /// 把已领任务转为「已完成」
    Close { claim_id: String },
    /// 查看本地领取台账
    Status,
    /// 生成给 agent 的调用文案(中文准本 / 英文省 token 版 + 已启用 skill 名字列表，不含 skill 全文)
    Prompt {
        /// 项目 id。省略则按 .tba/project 绑定解析；和 --name 二选一
        #[arg(long)]
        project: Option<String>,
        /// 项目名。给了但项目不存在就新建（幂等）。--project 优先于 --name
        #[arg(long)]
        name: Option<String>,
        /// 本次任务描述。给了就写进文案，agent 会新建任务直接开工；不给就让 agent 去队列领
        #[arg(long)]
        task: Option<String>,
        /// 本次任务正文文件(markdown)。和 --task 同时给时，正文以文件为准
        #[arg(long)]
        task_file: Option<String>,
        /// 文案语言：zh=中文(准本，默认) / en=英文(压缩版，省 token)
        #[arg(long, default_value = "zh")]
        lang: String,
        /// 只打印「项目接入文案」：让 agent 先扫仓库列出候选项目名、由用户拍板，再建项目。
        /// 不解析也不新建项目。--name 给了就固定用那个名字
        #[arg(long)]
        init: bool,
    },
    /// 项目共享记忆（存在 Teambition「项目记忆」任务的备注里，云端跨设备）
    Memory {
        #[arg(long)]
        project: String,
        /// 追加一条到某节（默认「踩坑」）
        #[arg(long)]
        append: Option<String>,
        /// 从文件读内容追加（长文本用，和 --append 二选一）
        #[arg(long)]
        file: Option<String>,
        /// 分节：红线 / 决策 / 踩坑（默认「踩坑」）
        #[arg(long, default_value = "")]
        section: String,
        /// 覆盖整节：归档该节的旧记忆任务，再写一条新的（需配 --force）
        #[arg(long)]
        overwrite: Option<String>,
        /// 红线/决策是价值判断，只有用户能改：写它们或覆盖整段需要这个
        #[arg(long)]
        force: bool,
        /// 打开本项目的 tb 共享记忆
        #[arg(long)]
        on: bool,
        /// 关闭本项目的 tb 共享记忆（记忆任务仍留在 Teambition 上）
        #[arg(long)]
        off: bool,
        /// 看状态：开关 / 记忆状态 / 各节条数与字数
        #[arg(long)]
        status: bool,
        /// 读旧的本地记忆目录（迁移前的 data/projects/<id>/memory/）
        #[arg(long)]
        local: bool,
        /// 把本地旧目录内容一次性并进云端记忆（不删本地）
        #[arg(long)]
        import_local: bool,
        /// 建记忆状态（mem）并检查（幂等，可反复跑）
        #[arg(long)]
        init: bool,
    },
    /// 查看 MCP 工具列表和角色识别结果
    Tools {
        #[arg(long)]
        refresh: bool,
        /// 输出每个工具的完整 inputSchema(JSON)。排查参数问题时用
        #[arg(long)]
        json: bool,
        /// 只输出 [tool_overrides] 片段：识别不对时贴进 data/config.toml
        #[arg(long)]
        overrides: bool,
    },
    /// 以 MCP server (stdio) 运行。给 agent 挂载用
    Serve,
    /// 打开 egui 窗口
    Ui,
    /// 截图存 png。BitBlt 抓屏，后台窗口也能截，不用把窗口提到前台
    Shot {
        /// 输出路径。目录不存在会自动建
        #[arg(long, default_value = "shot.png")]
        out: String,
        /// 窗口标题（包含匹配，取第一个可见窗口）。不给就截全屏
        #[arg(long)]
        title: Option<String>,
        /// 直接给窗口句柄，优先于 --title
        #[arg(long)]
        hwnd: Option<isize>,
        /// 压缩级别: fast(默认，毫秒级) | balanced | high(更小更慢)。都是无损
        #[arg(long, default_value = "fast")]
        level: String,
    },
    /// 通用能力：git。status 看状态 / main(别名 update) 提交并推送 /
    /// create 建 GitHub 仓库并推送（不用装 gh）/ tag 打标签
    Git {
        /// status | main | update | create | tag
        #[arg(default_value = "status")]
        action: String,
        /// main 的提交信息
        #[arg(long, short = 'm')]
        message: Option<String>,
        /// 推到哪个分支（默认 origin/HEAD 指向的，再默认 main）
        #[arg(long)]
        branch: Option<String>,
        /// GitButler 项目里额外跑 but land，直接落到目标分支
        #[arg(long)]
        land: bool,
        /// tag 动作的参数，如 v0.2.0（可省略 v）
        #[arg(long)]
        tag: Option<String>,
        /// create 用：仓库名。缺省用当前目录名（中文/空格洗成 -，全中文要显式给名）
        #[arg(long)]
        name: Option<String>,
        /// create 用：建私有仓库（默认公开）
        #[arg(long)]
        private: bool,
        /// create 用：远端名，默认 origin
        #[arg(long, default_value = "origin")]
        remote: String,
        /// 只打印将要执行的命令，不执行
        #[arg(long)]
        dry: bool,
    },
    /// 通用能力：发版。打 tag -> 推 -> (--watch) 等 CI 并列出产物。
    /// 注意：`release <claim_id>` 是「退回队列」，发版是 `publish`
    #[command(name = "publish", visible_alias = "rel")]
    Publish {
        /// 版本号或 tag，如 0.2.0 / v0.2.0。缺省读 Cargo.toml 的 version
        #[arg(long)]
        tag: Option<String>,
        /// 等 CI 跑完并列出产物（要 GitHub token：GH_TOKEN 或 git 凭据）
        #[arg(long)]
        watch: bool,
        /// owner/repo。缺省从 origin 解析
        #[arg(long)]
        repo: Option<String>,
        /// 只打 tag，不推送
        #[arg(long)]
        no_push: bool,
        /// 只生成 .github/workflows/release.yml（windows x64 + arm64 MSVC）
        #[arg(long)]
        init: bool,
        /// --init 时覆盖已存在的 workflow
        #[arg(long)]
        force: bool,
        /// --watch 最多等多少秒（默认 300，别超过 shell 工具上限）
        #[arg(long, default_value = "300")]
        timeout: u64,
    },
    /// 删除 skill（归档那条任务，Teambition 没有硬删除）
    SkillRm {
        #[arg(long)]
        project: String,
        #[arg(long)]
        name: String,
    },
    /// 内置模板已移到本地能力库：这里只做指路（tba cap list）
    SkillTpl,
    /// 通用能力（本地、跨项目）：增删改查 + 开关。agent 用 list 看目录、用名字读正文
    #[command(name = "cap", visible_alias = "caps")]
    Cap {
        /// list | show | add | edit | rm | on | off | restore | path。
        /// 缺省 list；也可以直接写能力名（等同 show）
        action: Option<String>,
        /// 能力名（show / add / edit / rm / on / off / restore 用）
        name: Option<String>,
        /// 一句话说明（写「什么时候用」；开工文案里一行一条给 agent 看）
        #[arg(long)]
        summary: Option<String>,
        /// 英文说明（英文开工文案用；空着回落到 --summary）
        #[arg(long = "summary-en")]
        summary_en: Option<String>,
        /// 正文：文件路径，或 `-` 读 stdin。add 不给留空，edit 不给保留原文
        #[arg(long)]
        body: Option<String>,
        /// 新建/修改时把这条能力关掉
        #[arg(long)]
        off: bool,
        /// 常驻：开工文案里标【常驻】，给「动手前就得知道」的红线用（如禁止弹命令行）
        #[arg(long)]
        always: bool,
        /// 取消常驻
        #[arg(long = "no-always")]
        no_always: bool,
    },
    /// 自检：令牌 / PATH / git / GitButler / 字体 / 轮询参数一次全列出来
    Doctor,
    /// 编译 & 重载（参数优先读项目设置：Teambition 状态为 setting 的任务）
    Reload {
        /// build | restart | kill | status
        #[arg(default_value = "build")]
        action: String,
        /// 项目 id。缺省读当前目录的 .tba/project 绑定
        #[arg(long)]
        project: Option<String>,
        /// 目标项目根目录。缺省读项目设置，再缺省按 exe 位置推断
        #[arg(long)]
        root: Option<String>,
        /// exe 基名。缺省读项目设置 / Cargo.toml
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
        /// 编译指令（覆盖项目设置里的）。zig/go/npm 项目用
        #[arg(long)]
        compile: Option<String>,
        /// 杀旧/启动走 UAC 提权
        #[arg(long)]
        elevate: bool,
    },
    /// 项目设置：编译指令 / 项目目录 / exe 名 / 启动参数（存 Teambition 的 setting 任务）
    Setting {
        /// show | set | guess | text
        #[arg(default_value = "show")]
        action: String,
        /// 项目 id。缺省读当前目录的 .tba/project 绑定
        #[arg(long)]
        project: Option<String>,
        /// 编译指令。空串 = 清空，让 agent 重新探测
        #[arg(long)]
        compile: Option<String>,
        /// 项目根目录
        #[arg(long)]
        root: Option<String>,
        /// exe 基名
        #[arg(long)]
        name: Option<String>,
        /// rename | copy
        #[arg(long)]
        mode: Option<String>,
        /// copy 模式的运行副本路径
        #[arg(long)]
        live: Option<String>,
        /// 启动参数
        #[arg(long)]
        args: Option<String>,
        /// 启用「编译 & 重载」（默认就是启用）
        #[arg(long)]
        on: bool,
        /// 停用「编译 & 重载」：agent 文案里不再出现
        #[arg(long)]
        off: bool,
        /// text 用：zh（默认）| en
        #[arg(long)]
        lang: Option<String>,
    },
}

fn main() {
    // 双击 exe（不带子命令）或显式 `tba ui` = 窗口模式，控制台黑窗要立刻消失。
    //
    // 为什么放在最前面：以前这一步在 ui::run 里才做，前面还有 ensure_dirs、beat_agent、
    // clap 解析——冷启动碰上杀软扫盘时能拖到几百毫秒，黑窗就那么挂着，看着就是
    // 「双击 tba 弹出命令行」。现在进程一进 main 就关，能省多少是多少。
    if gui_intent() {
        crate::ui::hide_console();
    }
    if let Err(e) = crate::cmds::run() {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}

/// 这次启动是不是「开窗口」：没有子命令，或者只有 `ui` 一个子命令。
fn gui_intent() -> bool {
    let mut args = std::env::args_os().skip(1);
    match args.next() {
        None => true,
        Some(a) => a == "ui" && args.next().is_none(),
    }
}
