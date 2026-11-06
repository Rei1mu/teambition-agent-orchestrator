use crate::ops::{
    agent_prompt_from, archive_skill, draft_for_mode, ensure_memory_state, export_skill_mem,
    list_skills, open_folder, push_skill, CopyMode, Lang, SkillItem,
};
use crate::caps::{self, Cap};
use crate::oauth::TOKEN_PAGE;
use crate::store::Store;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

// 默认字号。egui 自带约 14pt，中文在这个尺寸下很吃力，所以默认给 18。
const DEFAULT_FONT_SIZE: f32 = 18.0;
const MIN_FONT_SIZE: f32 = 12.0;
const MAX_FONT_SIZE: f32 = 40.0;
// 按默认字号算的布局基准，用来等比放大控件间距。
const BASE_FONT_SIZE: f32 = 14.0;

enum Ev {
    Projects(Vec<(String, String)>),
    Skills(String, Vec<SkillItem>),
    LoggedIn(usize),
    Err(String),
    /// 编译 & 重载的结果（项目 id, 输出）
    ReloadDone(String, String),
    /// 项目设置从云端读回来了（项目 id, 结果）
    SettingLoaded(String, Result<Option<crate::setting::ProjectSetting>, String>),
    /// 项目设置保存结果（项目 id, 结果）
    SettingSaved(String, Result<String, String>),
    /// GitButler/git 后台检测结果
    GbChecked {
        gb: crate::gitbutler::GbStatus,
        git: Option<String>,
    },
    /// GitButler CLI 后台安装结果
    GbInstalled(Result<(), String>),
    /// 测试邮件发送结果
    MailSent(Result<(), String>),
    /// 记忆分片初始化结果（含状态摘要）
    MemInit(Result<String, String>),
    /// 导出 skill / mem 到本地 skillmem 目录的结果
    SkillMem(Result<String, String>),
    /// skill 新建 / 编辑 / 删除的结果。带回项目 id，好重新拉一次列表。
    SkillSaved(String, Result<String, String>),
    /// 「接入项目」页：扫目录得到的候选项目名
    CreateScanned(Vec<crate::create::Row>),
    /// 「接入项目」页：建/复用项目的结果（输出全文，直接显示在结果框里）
    Created(Result<String, String>),
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Projects,
    /// 通用能力：本地、跨项目的文本能力库，可增删改查
    Caps,
    Settings,
    /// 接入项目：选目录 -> 列候选名 -> 点一个 -> 建项目 + 写绑定 + 开工文案
    Create,
}

// 界面偏好。单独存 data/ui.toml，不碰 config.toml（那里有 token 和工具映射，
// 整体覆写风险太大）。
#[derive(Clone, Serialize, Deserialize)]
struct UiPrefs {
    #[serde(default = "default_font_size")]
    font_size: f32,
    /// 中文字体 key（见 FONT_CHOICES）。空 = 自动（雅黑优先）。
    /// 存 key 不存路径：路径换机器就失效，key 还能回落。
    #[serde(default)]
    font: String,
}

fn default_font_size() -> f32 {
    DEFAULT_FONT_SIZE
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            font_size: DEFAULT_FONT_SIZE,
            font: String::new(),
        }
    }
}

fn prefs_path(store: &Store) -> PathBuf {
    store.root.join("ui.toml")
}

fn load_prefs(store: &Store) -> UiPrefs {
    match std::fs::read_to_string(prefs_path(store)) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => UiPrefs::default(),
    }
}

fn save_prefs(store: &Store, p: &UiPrefs) {
    if let Ok(s) = toml::to_string_pretty(p) {
        let _ = std::fs::write(prefs_path(store), s);
    }
}

pub struct App {
    store: Store,
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    // 没有 token 时显示引导面板
    needs_token: bool,
    // 离线模式：不用 Teambition，项目列表 / 能力 / 文案全走本地缓存。
    // 需要在设置页里切换，或 `TBA_OFFLINE=1` 强制打开。
    offline: bool,
    token_input: String,
    setup_busy: bool,
    projects: Vec<(String, String)>,
    skills: HashMap<String, Vec<SkillItem>>,
    // 每个项目卡片的「本次任务」草稿。留空 = 让 agent 去队列（列「未完成」）领。
    task_drafts: HashMap<String, String>,
    open: HashMap<String, bool>,
    loading_projects: bool,
    loading_skill: Option<String>,
    status: String,
    // 侧栏视图
    view: View,
    // 项目级：通用能力「不用」名单（本地 projects/<id>/caps-off.toml）
    cap_off: HashMap<String, Vec<String>>,
    // 项目「编译 & 重载」的编辑态（本地缓存 + 云端设置）
    setting: HashMap<String, crate::setting::ProjectSetting>,
    // 「编译 & 重载」的**文本**编辑缓冲（每个项目一段 TOML 原文）。
    // 表单拆成六个格子太碎，改一个参数得挨个找；直接编辑那段文本更顺手，
    // 而且它就是存到 Teambition 任务备注里的东西——所见即所存。
    compile_text: HashMap<String, String>,
    // 已经从云端拉过一次（避免每次展开都联网）
    setting_fetched: HashMap<String, bool>,
    // 正在读/写哪个项目的设置
    setting_busy: Option<String>,
    // 重载动作按项目记：编译要几十秒，切到别的项目也不该串台
    reload_busy: Option<String>,
    reload_out: HashMap<String, String>,
    reload_log: HashMap<String, String>,
    // 界面偏好（字号）
    font_size: f32,
    // 中文字体 key（空 = 自动）。改了要重装字体，所以记一份已生效的值做差异判断。
    font_choice: String,
    applied_font_choice: String,
    // 已经应用到 egui 的字号，避免每帧重复 set_style
    applied_font: f32,
    // GitButler 绑定（优先级高于 Teambition）：None = 还没检测完
    gb: Option<crate::gitbutler::GbStatus>,
    git_ver: Option<String>,
    gb_busy: bool,
    gb_msg: String,
    // 邮件配置表单（SMTP 发信 / IMAP 收信）。保存时 load-modify-save，不碰 token。
    mail: MailForm,
    mail_busy: bool,
    // 「待回复」等待与轮询参数。同样用表单存，避免输入中间态把 u64 解析搞崩。
    wait: WaitForm,
    // 记忆分片初始化在跑（建任务 + 写字段要联网，走后台线程）
    mem_busy: bool,
    // 正在导出 skill/mem 的项目 id（导出要联网拉 skill + 记忆，走后台线程）
    skillmem_busy: Option<String>,
    // skill 编辑窗。Some 时显示；保存/删除都在后台线程，写完重拉列表。
    skill_edit: Option<SkillEdit>,
    skill_saving: bool,
    // 通用能力：本地文件（data/caps.toml），读写都是同步的，不用后台线程。
    caps: Vec<Cap>,
    cap_edit: Option<CapEdit>,
    // 删除前的二次确认（存名字）
    cap_del: Option<String>,
    // 截图自验用：TBA_UI_OPEN=1 时所有项目卡片和折叠区默认展开
    // （GUI 没法用命令行点按钮，不展开就截不到里面的内容）。
    debug_open: bool,
    // 构建工具链状态。None = 还没算过（resolve 会跑 vswhere，不能每帧算）。
    // 改完路径就置 None，下一帧重算。
    tc: Option<crate::toolchain::Resolved>,
    // 只读文本查看窗（标题, 正文）。给「查看文案」这类「只想看一眼」的内容用。
    text_view: Option<(String, String)>,
    // 「接入项目」页。目录可以选任意位置，不必是窗口进程的 cwd。
    create_dir: String,
    create_rows: Vec<crate::create::Row>,
    create_pick: Option<usize>,
    create_custom: String,
    create_busy: bool,
    create_out: String,
}

// skill 编辑窗的编辑态。正文是整段 markdown，直接存 String。
#[derive(Clone)]
struct SkillEdit {
    project_id: String,
    project_name: String,
    name: String,
    content: String,
    // true = 新建（保存时若同名已存在，push_skill 会当成更新）
    is_new: bool,
}

// 通用能力编辑窗的编辑态。on / builtin 是原值，保存时原样带回——
// 编辑正文不该顺手把用户关掉的开关打开，也不该把内置标记丢掉。
#[derive(Clone)]
struct CapEdit {
    name: String,
    summary: String,
    summary_en: String,
    body: String,
    on: bool,
    builtin: bool,
    always: bool,
    is_new: bool,
    err: String,
}

impl CapEdit {
    fn new() -> CapEdit {
        CapEdit {
            name: String::new(),
            summary: String::new(),
            summary_en: String::new(),
            body: String::new(),
            on: true,
            builtin: false,
            always: false,
            is_new: true,
            err: String::new(),
        }
    }

    fn from(c: &Cap) -> CapEdit {
        CapEdit {
            name: c.name.clone(),
            summary: c.summary.clone(),
            summary_en: c.summary_en.clone(),
            body: c.body.clone(),
            on: c.on,
            builtin: c.builtin,
            always: c.always,
            is_new: false,
            err: String::new(),
        }
    }
}

/// Windows SDK 官方下载页：本机没装 SDK 时给个能点的地方，别让人自己搜。
const SDK_URL: &str = "https://developer.microsoft.com/windows/downloads/windows-sdk/";

/// VS 生成工具（Build Tools）：只装编译器 + Windows SDK，不带 IDE。
/// 比装完整 VS 轻，装的时候勾「使用 C++ 的桌面开发」就有 link.exe。
const VS_BUILD_TOOLS_URL: &str = "https://visualstudio.microsoft.com/visual-cpp-build-tools/";

/// 给编辑类弹窗开成一个真正的系统窗口。
///
/// 以前用 `egui::Window` 画在主窗口内部：主窗一缩小，编辑框就被裁掉；切到别的页面
/// 还会被后面的面板盖住，用户点完「编辑」经常找不到窗口。换成独立窗口后它有自己的
/// 标题栏和任务栏条目，位置对着主窗居中，关掉不影响主窗。
///
/// 用 immediate viewport：编辑态存在 App 里，回调里直接读写，不需要再搭一套跨窗口状态。
fn show_editor_window<R>(
    ctx: &egui::Context,
    key: &str,
    title: String,
    size: [f32; 2],
    min: [f32; 2],
    add: impl FnMut(&mut egui::Ui) -> R,
) -> R {
    let mut vb = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(size)
        .with_min_inner_size(min)
        .with_resizable(true);
    // 对着主窗口居中：主窗被拖到副屏时，编辑框也跟着过去，不会跑到看不见的地方。
    if let Some(r) = ctx.input(|i| i.viewport().outer_rect) {
        vb = vb.with_position(egui::pos2(
            r.center().x - size[0] * 0.5,
            r.center().y - size[1] * 0.5,
        ));
    }
    let mut add = add;
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(key), vb, |ctx, _| {
        let add = &mut add;
        egui::CentralPanel::default()
            .show(ctx, |ui| add(ui))
            .inner
    })
}

// 设置页的邮件表单。端口用 String 存，方便输入中间态（空/半个数字）。
#[derive(Clone, Default)]
struct MailForm {
    smtp_host: String,
    smtp_port: String,
    smtp_user: String,
    smtp_pass: String,
    smtp_from: String,
    mail_to: String,
    mail_on_ask: bool,
    imap_host: String,
    imap_port: String,
}

impl MailForm {
    fn from_cfg(cfg: &crate::config::Config) -> MailForm {
        MailForm {
            smtp_host: cfg.smtp_host.clone(),
            smtp_port: cfg.smtp_port.to_string(),
            smtp_user: cfg.smtp_user.clone(),
            smtp_pass: cfg.smtp_pass.clone(),
            smtp_from: cfg.smtp_from.clone(),
            mail_to: cfg.mail_to.clone(),
            mail_on_ask: cfg.mail_on_ask,
            imap_host: cfg.imap_host.clone(),
            imap_port: cfg.imap_port.to_string(),
        }
    }

    // 写回配置（只改邮件字段，其余原样保留）。
    fn apply_to(&self, cfg: &mut crate::config::Config) {
        cfg.smtp_host = self.smtp_host.trim().to_string();
        cfg.smtp_port = self
            .smtp_port
            .trim()
            .parse()
            .unwrap_or(crate::config::DEFAULT_SMTP_PORT);
        cfg.smtp_user = self.smtp_user.trim().to_string();
        cfg.smtp_pass = self.smtp_pass.clone();
        cfg.smtp_from = self.smtp_from.trim().to_string();
        cfg.mail_to = self.mail_to.trim().to_string();
        cfg.mail_on_ask = self.mail_on_ask;
        cfg.imap_host = self.imap_host.trim().to_string();
        cfg.imap_port = self
            .imap_port
            .trim()
            .parse()
            .unwrap_or(crate::config::DEFAULT_IMAP_PORT);
    }
}

// 设置页的等待/轮询表单。用 String 存，理由同上：输入「5」再输入「40」的中间态
// 不能当成非法值直接崩掉。
#[derive(Clone, Default)]
struct WaitForm {
    timeout_secs: String,
    interval_secs: String,
    mail_poll_secs: String,
}

impl WaitForm {
    fn from_cfg(cfg: &crate::config::Config) -> WaitForm {
        WaitForm {
            timeout_secs: cfg.wait_timeout_secs.to_string(),
            interval_secs: cfg.wait_interval_secs.to_string(),
            mail_poll_secs: cfg.mail_poll_secs.to_string(),
        }
    }

    // 空/非法就回落到默认值，别把配置写成 0；合法但过小的也夹到下限。
    fn apply_to(&self, cfg: &mut crate::config::Config) {
        cfg.wait_timeout_secs = self
            .timeout_secs
            .trim()
            .parse()
            .unwrap_or(crate::config::DEFAULT_WAIT_TIMEOUT_SECS)
            .max(crate::config::MIN_WAIT_TIMEOUT_SECS);
        cfg.wait_interval_secs = self
            .interval_secs
            .trim()
            .parse()
            .unwrap_or(crate::config::DEFAULT_WAIT_INTERVAL_SECS)
            .max(crate::config::MIN_WAIT_INTERVAL_SECS);
        cfg.mail_poll_secs = self
            .mail_poll_secs
            .trim()
            .parse()
            .unwrap_or(crate::config::DEFAULT_MAIL_POLL_SECS)
            .max(crate::config::MIN_MAIL_POLL_SECS);
    }
}

// 可选的中文字体。key 存进 ui.toml，label 给人看，path 是本机路径。
//
// 顺序就是默认回退顺序：黑体排第一（用户要求「中文用黑体」），
// 后面按覆盖度排，最后是符号字体（Segoe UI Symbol），专治 ⚠ ✓ ▾ 这类字符。
const FONT_CHOICES: &[(&str, &str, &str)] = &[
    ("simhei", "黑体 SimHei", "C:/Windows/Fonts/simhei.ttf"),
    ("msyh", "微软雅黑 Microsoft YaHei", "C:/Windows/Fonts/msyh.ttc"),
    ("deng", "等线 DengXian", "C:/Windows/Fonts/Deng.ttf"),
    ("simsun", "宋体 SimSun", "C:/Windows/Fonts/simsun.ttc"),
    ("msjh", "微軟正黑體 JhengHei", "C:/Windows/Fonts/msjh.ttc"),
    ("pingfang", "苹方 PingFang", "/System/Library/Fonts/PingFang.ttc"),
    (
        "notocjk",
        "Noto Sans CJK",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ),
];

// 符号字体放链尾。缺了它，界面上的 ⚠ ✓ 会渲染成豆腐块（口）。
const SYMBOL_FONT: (&str, &str) = ("seguisym", "C:/Windows/Fonts/seguisym.ttf");

fn font_path(key: &str) -> Option<&'static str> {
    FONT_CHOICES
        .iter()
        .find(|c| c.0 == key)
        .map(|c| c.2)
        .or_else(|| (SYMBOL_FONT.0 == key).then_some(SYMBOL_FONT.1))
}

// 装中文字体。
//
// 只装一个字体时，它缺的字形会渲染成豆腐块（口）——这就是界面上「好多口」的原因：
// 等线缺符号和 emoji，遇到 ⚠ ▾ ✓ 这类字符就没字形了。
// 所以这里按覆盖度排一条链：首选字体 -> 其余中文字体 -> Segoe UI Symbol。
// 内置英文字体留在链首，英文和数字仍用它渲染，中文落到链上的第一个有字形的字体。
fn install_cjk_font(ctx: &egui::Context, pref: &str) {
    let mut order: Vec<(&'static str, &'static str)> = Vec::new();
    if let Some(p) = font_path(pref) {
        if let Some(key) = FONT_CHOICES.iter().find(|c| c.0 == pref).map(|c| c.0) {
            order.push((key, p));
        }
    }
    for c in FONT_CHOICES {
        if c.0 != pref {
            order.push((c.0, c.2));
        }
    }
    order.push(SYMBOL_FONT);

    let mut fonts = egui::FontDefinitions::default();
    let mut loaded: Vec<&str> = Vec::new();
    for (key, path) in order {
        if loaded.contains(&key) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes.len() < 1024 {
            continue;
        }
        fonts.font_data.insert(
            key.to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        loaded.push(key);
    }
    if !loaded.is_empty() {
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let chain = fonts.families.entry(fam).or_default();
            for key in &loaded {
                if !chain.iter().any(|f| f == key) {
                    chain.push((*key).to_string());
                }
            }
        }
        eprintln!("[tb ui] 已加载字体: {}", loaded.join(", "));
    }
    ctx.set_fonts(fonts);
}

impl App {
    pub fn new(store: Store) -> App {
        let (tx, rx) = channel();
        // 先用缓存秒开，再后台刷新。
        let cached = store.load_projects_cache();
        // 离线模式：不要求 token，项目列表直接用上次同步下来的缓存。
        let offline = store.load_config().offline();
        let needs_token = !offline && store.load_config().token().is_none();
        let prefs = load_prefs(&store);
        let font_size = prefs.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        // 邮件表单要预填，先读一份配置（store 之后会被 move 进 App）。
        let store_cfg = store.load_config();
        // 通用能力是本地文件，窗口打开时读一次即可（不存在会灌一份内置模板）。
        let caps_rows = caps::list(&store);
        // 截图验证用：TBA_UI_CAP=shot 直接打开那条能力的编辑窗（GUI 没法用命令行点按钮）。
        let cap_edit0 = std::env::var("TBA_UI_CAP")
            .ok()
            .and_then(|n| caps::get(&store, &n))
            .map(|c| CapEdit::from(&c));
        // 同一个钩子家族：TBA_UI_TEXT=<项目 id> 直接打开「编译 & 重载」的文案查看窗。
        let text_view0 = std::env::var("TBA_UI_TEXT").ok().map(|pid| {
            let st = crate::setting::ProjectSetting::load_cached(&store, &pid)
                .unwrap_or_default();
            let exe = crate::ops::prompt_exe(&store);
            let block = st.prompt_block(&exe, true);
            let text = if block.trim().is_empty() {
                "「编译 & 重载」已停用：这段不会出现在开工文案里。".to_string()
            } else {
                block.trim_end().to_string()
            };
            ("编译 & 重载 · 开工文案".to_string(), text)
        });
        let mut app = App {
            store,
            tx,
            rx,
            needs_token,
            offline,
            token_input: String::new(),
            setup_busy: false,
            projects: if needs_token { Vec::new() } else { cached },
            skills: HashMap::new(),
            task_drafts: HashMap::new(),
            open: HashMap::new(),
            loading_projects: !needs_token,
            loading_skill: None,
            status: if needs_token {
                "还没有 Teambition 访问令牌".to_string()
            } else if offline {
                "离线模式：只用本地数据（项目列表、能力、文案都走缓存）".to_string()
            } else {
                "正在连接 Teambition...".to_string()
            },
            view: match std::env::var("TBA_UI_VIEW").as_deref() {
                // 截图验证用：GUI 没法用命令行点按钮，起个指定页才好自查界面。
                Ok("settings") => View::Settings,
                Ok("caps") => View::Caps,
                Ok("create") => View::Create,
                _ => View::Projects,
            },
            cap_off: HashMap::new(),
            setting: HashMap::new(),
            compile_text: HashMap::new(),
            setting_fetched: HashMap::new(),
            setting_busy: None,
            reload_busy: None,
            reload_out: HashMap::new(),
            reload_log: HashMap::new(),
            font_size,
            font_choice: prefs.font.clone(),
            applied_font_choice: prefs.font.clone(),
            applied_font: 0.0,
            gb: None,
            git_ver: None,
            gb_busy: false,
            gb_msg: String::new(),
            mail: MailForm::from_cfg(&store_cfg),
            mail_busy: false,
            wait: WaitForm::from_cfg(&store_cfg),
            mem_busy: false,
            skillmem_busy: None,
            skill_edit: None,
            skill_saving: false,
            caps: caps_rows,
            cap_edit: cap_edit0,
            cap_del: None,
            debug_open: std::env::var("TBA_UI_OPEN").as_deref() == Ok("1"),
            // 懒加载：设置页第一次打开时才算（要跑 vswhere，启动时不抢时间）。
            tc: None,
            text_view: text_view0,
            create_dir: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            create_rows: Vec::new(),
            create_pick: None,
            create_custom: String::new(),
            create_busy: false,
            create_out: String::new(),
        };
        if !needs_token {
            app.reload_projects();
        }
        // GitButler 检测放后台跑（要起子进程取版本/工作区状态），不挡窗口打开
        app.check_gitbutler();
        app
    }

    /// 后台检测 GitButler CLI + git。
    fn check_gitbutler(&mut self) {
        let store = self.store.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let gb = crate::gitbutler::detect(&store);
            let git = crate::gitbutler::git_available();
            let _ = tx.send(Ev::GbChecked { gb, git });
        });
    }

    /// 让用户手选 but.exe。选完写 config.toml 并重新检测。
    ///
    /// 为什么不做扫盘：扫 Program Files/盘符既慢（实测 1.3 秒起）又可能挑到旧副本，
    /// 让用户指一次最准；指过一次就记住，下次直接命中。
    fn pick_but_path(&mut self) {
        let start = self
            .gb
            .as_ref()
            .and_then(|g| g.path.as_ref())
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .filter(|d| d.is_dir())
            .unwrap_or_else(|| self.store.root.clone());
        let mut dlg = rfd::FileDialog::new()
            .set_title("选择 GitButler CLI（but.exe）")
            .set_directory(start);
        if cfg!(windows) {
            dlg = dlg.add_filter("but.exe", &["exe"]);
        }
        let Some(p) = dlg.pick_file() else {
            return; // 用户取消，什么都不改
        };
        match crate::gitbutler::set_path(&self.store, &p.display().to_string()) {
            Ok(msg) => {
                self.status = msg;
                self.gb = None;
                self.check_gitbutler();
            }
            Err(e) => self.status = format!("指定 but 失败: {e}"),
        }
    }

    /// 后台把 GitButler CLI 装到运行目录（下载 MSI 约 36MB + 抽取，比较慢）。
    fn install_gitbutler(&mut self) {
        self.gb_busy = true;
        self.gb_msg = "正在安装 GitButler CLI（下载约 36MB，稍候）...".to_string();
        let store = self.store.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let r = crate::gitbutler::install(&store).map(|_| ());
            let _ = tx.send(Ev::GbInstalled(r));
        });
    }

    /// 缺 GitButler / git 时的一句话提醒；都正常时返回 None（完全不占位）。
    /// 项目列表上方只留这一行，卡片本体在设置页。
    fn git_missing_hint(&self) -> Option<String> {
        let gb = self.gb.as_ref()?;
        if !gb.found {
            return Some(
                "没检测到 GitButler CLI（but.exe），TBHub 不会干活——去设置里装或选路径".to_string(),
            );
        }
        if self.git_ver.is_none() {
            return Some("没检测到 git，GitButler 依赖它".to_string());
        }
        None
    }

    // 「接入项目」页：选目录 -> 扫候选 -> 点一个 -> 建项目。
    // 扫描和创建都要联网，放后台线程；结果通过 Ev 回主线程，窗口不卡住。
    fn scan_create_candidates(&mut self) {
        let store = self.store.clone();
        let tx = self.tx.clone();
        let dir = std::path::PathBuf::from(self.create_dir.trim());
        self.create_busy = true;
        self.status = "正在扫描目录、核对 Teambition 同名项目...".to_string();
        thread::spawn(move || {
            let rows = crate::create::scan(&store, &dir, None);
            let _ = tx.send(Ev::CreateScanned(rows));
        });
    }

    fn do_create_project(&mut self, name: String) {
        let store = self.store.clone();
        let tx = self.tx.clone();
        let dir = std::path::PathBuf::from(self.create_dir.trim());
        self.create_busy = true;
        self.status = format!("正在接入「{name}」...");
        thread::spawn(move || {
            let r = crate::create::create(&store, &dir, &name, None, Lang::Zh)
                .map(|c| c.report());
            let _ = tx.send(Ev::Created(r));
        });
    }

    fn draw_create(&mut self, ui: &mut egui::Ui) {
        ui.heading("接入项目");
        ui.label("给一个还没接 Teambition 的仓库建项目。一条命令的窗口版：选目录、扫候选、点一个、直接建好。");
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("目录:");
            ui.add(
                egui::TextEdit::singleline(&mut self.create_dir)
                    .desired_width(f32::INFINITY),
            );
            if ui.button("选目录…").clicked() {
                let start = std::path::PathBuf::from(self.create_dir.trim());
                let mut dlg = rfd::FileDialog::new().set_title("选择要接入的仓库目录");
                if start.is_dir() {
                    dlg = dlg.set_directory(&start);
                }
                if let Some(p) = dlg.pick_folder() {
                    self.create_dir = p.display().to_string();
                    self.create_rows.clear();
                    self.create_pick = None;
                    self.create_out.clear();
                }
            }
        });

        let offline = self.offline;
        let need_token = self.needs_token;
        ui.horizontal(|ui| {
            let scan = ui.add_enabled(
                !self.create_busy,
                egui::Button::new("扫描候选项目名"),
            );
            if scan.clicked() {
                self.scan_create_candidates();
            }
            if ui.button("复制 CLI 命令").clicked() {
                ui.ctx().copy_text(format!(
                    "tba create --dir \"{}\"",
                    self.create_dir.trim()
                ));
                self.status = "已复制 tba create 命令".to_string();
            }
        });
        if offline || need_token {
            ui.colored_label(
                egui::Color32::from_rgb(0xE0, 0x7B, 0x39),
                "当前离线或没登录：可以扫候选名，但建项目需要先联网。",
            );
        }

        ui.separator();
        ui.label("候选项目名（点一个选中；名字要由你拍板，工具不会自己挑）：");
        if self.create_rows.is_empty() {
            ui.label(egui::RichText::new("还没扫描。点上面的「扫描候选项目名」。").weak());
        }
        let rows = self.create_rows.clone();
        for (i, r) in rows.iter().enumerate() {
            let label = format!("{}. {}    来源: {}    {}", i + 1, r.name, r.source, r.mark);
            if ui
                .selectable_label(self.create_pick == Some(i), label)
                .clicked()
            {
                self.create_pick = Some(i);
                self.create_custom.clear();
            }
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("或自己填名字:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.create_custom)
                    .hint_text("候选都不对时填这里")
                    .desired_width(240.0),
            );
            if resp.changed() {
                self.create_pick = None;
            }
        });
        let picked = self
            .create_pick
            .and_then(|i| self.create_rows.get(i))
            .map(|r| r.name.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let c = self.create_custom.trim().to_string();
                (!c.is_empty()).then_some(c)
            });

        ui.add_space(4.0);
        let can = !self.create_busy && !offline && !need_token && picked.is_some();
        let btn = ui.add_enabled(can, egui::Button::new("建 / 复用这个项目"));
        if btn.clicked() {
            if let Some(name) = picked {
                self.do_create_project(name);
            }
        }
        if let Some(name) = self
            .create_pick
            .and_then(|i| self.create_rows.get(i))
            .map(|r| r.name.as_str())
        {
            ui.label(format!("将使用: {name}"));
        }
        if self.create_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("处理中…");
            });
        }

        if !self.create_out.is_empty() {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("结果 / 开工文案").strong());
                if ui.button("复制全文").clicked() {
                    ui.ctx().copy_text(self.create_out.clone());
                    self.status = "已复制接入结果和开工文案".to_string();
                }
                if ui.button("清空").clicked() {
                    self.create_out.clear();
                }
            });
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.create_out)
                            .desired_width(f32::INFINITY)
                            .code_editor(),
                    );
                });
        }
    }

    /// GitButler 状态卡：CLI 装没装、什么版本、从哪来、当前目录配没配；
    /// 缺的时候给「安装到运行目录」一键装。git 是它的系统级依赖，一起显示。
    fn draw_gitbutler_card(&mut self, ui: &mut egui::Ui) {
        // 先克隆出来，避免闭包里同时借 self 和改 self
        let gb = self.gb.clone();
        let mut want_check = false;
        let mut want_install = false;
        let mut want_pick = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("GitButler").strong());
                match &gb {
                    None => {
                        ui.spinner();
                        ui.label("检测中...");
                    }
                    Some(g) if g.found => {
                        let ver = g.version.clone().unwrap_or_else(|| "版本未知".to_string());
                        // 跑不出 --version 多半是选错了文件（比如桌面版主程序），标黄别标绿。
                        let (mark, color) = if g.version.is_some() {
                            ("✓", egui::Color32::from_rgb(0x2e, 0x7d, 0x32))
                        } else {
                            ("!", egui::Color32::from_rgb(0xb2, 0x6a, 0x00))
                        };
                        let resp = ui.colored_label(color, format!("{mark} but {ver}"));
                        if let Some(p) = &g.path {
                            resp.on_hover_text(format!("路径: {}", p.display()));
                        }
                        ui.label(format!("来源: {}", g.source.map(|s| s.label()).unwrap_or("?")));
                        if ui
                            .small_button("指定位置")
                            .on_hover_text("自动检测不对？手动选一次 but.exe，记住后下次直接用")
                            .clicked()
                        {
                            want_pick = true;
                        }
                        // 这里以前显示「当前目录未配置（but setup）」，纯属误导：
                        // GUI 是全局窗口，它的启动目录（快捷方式/开始菜单）不是任何项目目录，
                        // 这个状态既不该在窗口里看、也没法在这儿修。真正用到 but 的是
                        // `tba git`，它按你执行命令的那个项目目录自己判断。
                        if ui.small_button("重新检测").clicked() {
                            want_check = true;
                        }
                    }
                    Some(_) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(0xc6, 0x28, 0x28),
                            "✗ 未检测到 GitButler CLI",
                        );
                        ui.label("（优先级高于 Teambition，agent 靠它管分支/提交）");
                        ui.label(
                            egui::RichText::new(
                                "已找过：环境变量 TBA_BUT / data/bin / PATH / 常见安装目录（不扫盘）",
                            )
                            .small()
                            .weak(),
                        );
                    }
                }
            });
            // git 依赖（系统级，不自动装，只引导）
            if self.git_ver.is_none() && gb.is_some() {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xc6, 0x28, 0x28),
                        "✗ 未检测到 git（GitButler CLI 依赖它）",
                    );
                    if ui.small_button("复制下载页").clicked() {
                        ui.ctx()
                            .copy_text(crate::gitbutler::GIT_DOWNLOAD_URL.to_string());
                    }
                });
            } else if let Some(v) = &self.git_ver {
                ui.label(format!("git {v}"));
            }
            // 手动安装入口**常显**：已经装好的人也可能要重装、升级、装到另一台机器，
            // 或者装完才发现版本不对。只在「缺 CLI」时给按钮，等于装过的人想看下载地址都看不到。
            ui.horizontal_wrapped(|ui| {
                ui.label("手动安装：");
                if ui
                    .button("GitButler 下载页")
                    .on_hover_text(format!(
                        "{}\n已装也能用：重装 / 升级 / 换机器",
                        crate::gitbutler::GB_DOWNLOAD_URL
                    ))
                    .clicked()
                {
                    crate::proc::open_url(crate::gitbutler::GB_DOWNLOAD_URL);
                }
                if ui
                    .button("git 下载页")
                    .on_hover_text(format!(
                        "{}\n已装也能用：重装 / 升级 / 换机器",
                        crate::gitbutler::GIT_DOWNLOAD_URL
                    ))
                    .clicked()
                {
                    crate::proc::open_url(crate::gitbutler::GIT_DOWNLOAD_URL);
                }
                if ui
                    .button("选择 but.exe 位置")
                    .on_hover_text("装了但装在别处、或自动检测认错了？手动选一次，记到 config.toml")
                    .clicked()
                {
                    want_pick = true;
                }
                if ui
                    .add_enabled(
                        !self.gb_busy,
                        egui::Button::new("安装到运行目录 (data/bin)"),
                    )
                    .on_hover_text("下载官方 MSI 并抽出 but.exe，无需管理员、不动注册表；已装时可用来升级")
                    .clicked()
                {
                    want_install = true;
                }
                if ui
                    .button("复制官方安装命令")
                    .on_hover_text("官方一键命令仅 mac/linux 可用；Windows 走上面那个 MSI 抽取")
                    .clicked()
                {
                    ui.ctx()
                        .copy_text(crate::gitbutler::OFFICIAL_INSTALL_CMD.to_string());
                }
            });
            if gb.as_ref().map(|g| !g.found).unwrap_or(false) {
                ui.label(
                    egui::RichText::new(
                        "Windows 官方只发 MSI：点「安装到运行目录」会自动下载并抽取 but.exe 到 data/bin，\
                         无需管理员、不动注册表。",
                    )
                    .small()
                    .weak(),
                );
            }
            if self.gb_busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(&self.gb_msg);
                });
            } else if !self.gb_msg.is_empty() {
                ui.label(egui::RichText::new(&self.gb_msg).small().weak());
            }
        });
        if want_check {
            self.check_gitbutler();
        }
        if want_install {
            self.install_gitbutler();
        }
        if want_pick {
            self.pick_but_path();
        }
    }

    fn reload_projects(&mut self) {
        // 离线：不连 Teambition，直接用上次同步下来的项目列表。
        // 「刷新」在这里的含义变成「重读缓存」，语义上说得通，按钮就不用换文案。
        if self.offline {
            self.projects = self.store.load_projects_cache();
            self.loading_projects = false;
            self.status = if self.projects.is_empty() {
                "离线模式：本地还没有项目缓存。联网打开一次（或在设置里关掉离线）就会存下来。"
                    .to_string()
            } else {
                format!("离线模式：{} 个项目（本地缓存）", self.projects.len())
            };
            return;
        }
        self.loading_projects = true;
        self.status = "正在连接 Teambition...".to_string();
        self.skills.clear();
        let store = self.store.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut tb = match crate::tb::Tb::new(store.clone()) {
                Ok(t) => t,
                Err(e) => {
                    let _ = tx.send(Ev::Err(e));
                    return;
                }
            };
            if let Err(e) = tb.discover(false) {
                let _ = tx.send(Ev::Err(e));
                return;
            }
            let overrides = tb.cfg.tool_overrides.clone();
            let map = crate::tools::ToolMap::resolve(&tb.tools, &overrides);
            let mut ctx = crate::ops::Ctx { tb, map };
            match ctx.list_projects() {
                Ok(ps) => {
                    let list: Vec<(String, String)> =
                        ps.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
                    let _ = store.save_projects_cache(&list);
                    let _ = tx.send(Ev::Projects(list));
                }
                Err(e) => {
                    let _ = tx.send(Ev::Err(e));
                }
            }
        });
    }

    fn load_skills(&mut self, project_id: &str) {
        if self.skills.contains_key(project_id) {
            return;
        }
        if self.loading_skill.as_deref() == Some(project_id) {
            return;
        }
        self.loading_skill = Some(project_id.to_string());
        // 离线：能力读本地缓存。能力本来就是「一段提示词 + 一个开关」，
        // 勾选和拼装全在本地，只有存储依赖 Teambition。
        if self.offline {
            let items = crate::ops::cached_skills(&self.store, project_id);
            let n = items.len();
            self.skills.insert(project_id.to_string(), items);
            self.loading_skill = None;
            if n == 0 {
                self.status = "离线模式：这个项目还没有能力缓存。联网打开一次就会存下来。"
                    .to_string();
            }
            return;
        }
        let store = self.store.clone();
        let tx = self.tx.clone();
        let pid = project_id.to_string();
        thread::spawn(move || match list_skills(&store, &pid) {
            Ok(items) => {
                let _ = tx.send(Ev::Skills(pid, items));
            }
            Err(e) => {
                let _ = tx.send(Ev::Err(e));
            }
        });
    }

    // 保存并验证 token。失败则把 token 清掉，留在引导面板。
    fn save_token(&mut self, token: String) {
        let token = token.trim().to_string();
        if token.is_empty() {
            return;
        }
        self.setup_busy = true;
        self.status = "正在验证 token...".to_string();
        let store = self.store.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut cfg = store.load_config();
            cfg.access_token = Some(token);
            if let Err(e) = store.save_config(&cfg) {
                let _ = tx.send(Ev::Err(e));
                return;
            }
            match crate::oauth::verify(&store) {
                Ok(n) => {
                    let _ = tx.send(Ev::LoggedIn(n));
                }
                Err(e) => {
                    let mut cfg = store.load_config();
                    cfg.access_token = None;
                    let _ = store.save_config(&cfg);
                    let _ = tx.send(Ev::Err(e));
                }
            }
        });
    }

    fn logout(&mut self) {
        let mut cfg = self.store.load_config();
        cfg.access_token = None;
        let _ = self.store.save_config(&cfg);
        self.needs_token = true;
        self.token_input.clear();
        self.projects.clear();
        self.skills.clear();
        self.open.clear();
        self.loading_projects = false;
        self.status = "已退出登录。请重新填入 Token。".to_string();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 收后台事件
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Ev::Projects(list) => {
                    self.projects = list;
                    self.loading_projects = false;
                    self.status = format!("共 {} 个项目", self.projects.len());
                }
                Ev::Skills(pid, rows) => {
                    self.skills.insert(pid.clone(), rows);
                    if self.loading_skill.as_deref() == Some(pid.as_str()) {
                        self.loading_skill = None;
                    }
                }
                Ev::LoggedIn(n) => {
                    self.needs_token = false;
                    self.setup_busy = false;
                    self.status = format!("已登录，发现 {n} 个 MCP 工具。正在加载项目...");
                    self.reload_projects();
                }
                Ev::Err(e) => {
                    self.loading_projects = false;
                    self.loading_skill = None;
                    self.setup_busy = false;
                    if self.needs_token {
                        self.status = format!("Token 无效或连接失败: {e}");
                    } else {
                        self.status = format!("错误: {e}");
                    }
                }
                Ev::ReloadDone(pid, text) => {
                    self.reload_busy = None;
                    if let Some(st) = self.setting.get(&pid) {
                        self.reload_log
                            .insert(pid.clone(), crate::capability::log_tail(&st.root, 15));
                    }
                    self.reload_out.insert(pid, text);
                }
                Ev::SettingLoaded(pid, r) => {
                    self.setting_busy = None;
                    match r {
                        Ok(Some(st)) => {
                            // 云端拉回来就重置文本缓冲：这是用户主动「从云端重读」，
                            // 或者第一次展开时的自动拉取（那时还没人编辑过）。
                            self.compile_text.insert(pid.clone(), st.to_note());
                            self.setting.insert(pid, st);
                        }
                        Ok(None) => {
                            self.status = if self.offline {
                                "离线模式：这个项目还没有「编译 & 重载」设置。填好后保存会写本地缓存\
                                 （data/projects/<id>/setting.toml），联网时再同步到 Teambition。"
                                    .to_string()
                            } else {
                                "还没有「编译 & 重载」设置：填好点「保存到 Teambition」，\
                                 或让 agent 跑 `tba setting set --compile ...`"
                                    .to_string()
                            };
                        }
                        Err(e) => self.status = format!("读取项目设置失败: {e}"),
                    }
                }
                Ev::SettingSaved(pid, r) => {
                    self.setting_busy = None;
                    match r {
                        Ok(msg) => {
                            self.status = msg;
                            // 保存成功顺手把内存里的值对齐（fill_defaults 可能补了字段）
                            if let Some(st) =
                                crate::setting::ProjectSetting::load_cached(&self.store, &pid)
                            {
                                // 文本也换成规范化后的版本：用户手写的引号/转义被修正，
                                // 下次打开看到的就是「实际存下去的东西」。
                                self.compile_text.insert(pid.clone(), st.to_note());
                                self.setting.insert(pid, st);
                            }
                        }
                        Err(e) => self.status = format!("保存项目设置失败: {e}"),
                    }
                }
                Ev::GbChecked { gb, git } => {
                    self.gb = Some(gb);
                    self.git_ver = git;
                }
                Ev::GbInstalled(r) => {
                    self.gb_busy = false;
                    match r {
                        Ok(()) => {
                            self.gb_msg = "安装完成，正在重新检测...".to_string();
                            self.check_gitbutler();
                        }
                        Err(e) => {
                            self.gb_msg = format!("安装失败: {e}");
                        }
                    }
                }
                Ev::MailSent(r) => {
                    self.mail_busy = false;
                    self.status = match r {
                        Ok(()) => "测试邮件已发出，去收件箱看看（没收到就看垃圾箱/端口是否 465/587）。"
                            .to_string(),
                        Err(e) => format!("测试邮件发送失败: {e}"),
                    };
                }
                Ev::MemInit(r) => {
                    self.mem_busy = false;
                    self.status = match r {
                        Ok(s) => format!("记忆分片已就绪\n{s}"),
                        Err(e) => format!("初始化记忆分片失败: {e}"),
                    };
                }
                Ev::SkillMem(r) => {
                    self.skillmem_busy = None;
                    self.status = match r {
                        Ok(s) => s,
                        Err(e) => format!("导出 skill/mem 失败: {e}"),
                    };
                }
                Ev::SkillSaved(pid, r) => {
                    self.skill_saving = false;
                    match r {
                        Ok(msg) => {
                            self.status = msg;
                            self.skill_edit = None;
                            // 重新拉一次，让新内容立刻出现在列表里。
                            self.skills.remove(&pid);
                            self.load_skills(&pid);
                        }
                        Err(e) => {
                            self.status = format!("保存 skill 失败: {e}");
                        }
                    }
                }
                Ev::CreateScanned(rows) => {
                    self.create_rows = rows;
                    self.create_pick = None;
                    self.create_busy = false;
                    self.status = format!("扫出 {} 个候选项目名，点一个再建", self.create_rows.len());
                }
                Ev::Created(r) => {
                    self.create_busy = false;
                    match r {
                        Ok(out) => {
                            self.create_out = out;
                            self.status = "项目已接入。开工文案在下面，可直接复制。".to_string();
                            self.reload_projects();
                        }
                        Err(e) => {
                            self.create_out = e.clone();
                            self.status = format!("接入失败: {e}");
                        }
                    }
                }
            }
        }
        // 后台在工作时保持界面响应
        if self.loading_projects
            || self.loading_skill.is_some()
            || self.setup_busy
            || self.reload_busy.is_some()
            || self.setting_busy.is_some()
            || self.skillmem_busy.is_some()
            || self.create_busy
        {
            ctx.request_repaint();
        }
        if self.mail_busy {
            ctx.request_repaint();
        }
        if self.mem_busy {
            ctx.request_repaint();
        }
        if self.gb_busy || self.gb.is_none() {
            ctx.request_repaint();
        }

        // 字体选择变了就重装字体链。set_fonts 有代价（要重建字形图集），
        // 所以只在 key 真的变了才做，不能每帧来一次。
        if self.applied_font_choice != self.font_choice {
            install_cjk_font(ctx, &self.font_choice);
            self.applied_font_choice = self.font_choice.clone();
        }

        // 字号变了才重设样式。egui 默认约 14pt，中文在这个尺寸下很吃力。
        if (self.applied_font - self.font_size).abs() > 0.01 {            let s = self.font_size;
            let mut style = (*ctx.style()).clone();
            style.text_styles = [
                (
                    egui::TextStyle::Heading,
                    egui::FontId::new(s * 1.5, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Body,
                    egui::FontId::new(s, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Button,
                    egui::FontId::new(s, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Small,
                    egui::FontId::new(s * 0.78, egui::FontFamily::Proportional),
                ),
                (
                    egui::TextStyle::Monospace,
                    egui::FontId::new(s * 0.9, egui::FontFamily::Monospace),
                ),
            ]
            .into();
            // 控件间距跟着等比放大，否则大字会挤在一起。
            let k = s / BASE_FONT_SIZE;
            style.spacing.item_spacing = egui::vec2(8.0 * k, 6.0 * k);
            style.spacing.button_padding = egui::vec2(8.0 * k, 4.0 * k);
            style.spacing.interact_size.y = 20.0 * k;
            ctx.set_style(style);
            self.applied_font = s;
        }
        let scale = self.font_size / BASE_FONT_SIZE;
        let nav_width = (150.0 * scale).clamp(150.0, 260.0);

        // 左侧导航：项目 / 通用能力 / 设置
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(nav_width)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.heading("tba");
                ui.add_space(10.0);

                ui.label(egui::RichText::new("项目").small().weak());
                if ui
                    .selectable_label(self.view == View::Projects, "项目列表")
                    .clicked()
                {
                    self.view = View::Projects;
                }

                if ui
                    .selectable_label(self.view == View::Create, "接入项目")
                    .on_hover_text("选目录 -> 列候选项目名 -> 点一个 -> 直接建好并出开工文案")
                    .clicked()
                {
                    self.view = View::Create;
                }

                ui.add_space(12.0);
                ui.label(egui::RichText::new("通用能力").small().weak());
                if ui
                    .selectable_label(self.view == View::Caps, "能力（可编辑）")
                    .on_hover_text("本地、跨项目的文本能力库：增删改查 + 开关")
                    .clicked()
                {
                    self.view = View::Caps;
                }

                ui.add_space(12.0);
                ui.label(egui::RichText::new("设置").small().weak());
                if ui
                    .selectable_label(self.view == View::Settings, "设置")
                    .on_hover_text("字体大小等界面偏好")
                    .clicked()
                {
                    self.view = View::Settings;
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(8.0);
                    if self.offline {
                        ui.colored_label(
                            egui::Color32::from_rgb(0xb2, 0x6a, 0x00),
                            egui::RichText::new("离线").small(),
                        )
                        .on_hover_text(
                            "离线模式：不连 Teambition，项目列表 / 能力 / 文案都走本地缓存。\
                             在设置页可切换",
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(if self.needs_token {
                                "未登录"
                            } else {
                                "已登录"
                            })
                            .small()
                            .weak(),
                        );
                    }
                });
            });

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("tba · 项目与能力");
                if self.offline {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xb2, 0x6a, 0x00),
                        egui::RichText::new("离线").strong(),
                    )
                    .on_hover_text(
                        "不用 Teambition：项目列表和能力都读本地缓存，开工文案照常生成。\
                         领任务 / 完成任务这类要云端才成立的操作会说明不可用。",
                    );
                }
                if !self.needs_token {
                    if ui
                        .button(if self.offline { "重读缓存" } else { "刷新" })
                        .clicked()
                    {
                        self.reload_projects();
                    }
                    if ui
                        .add_enabled(!self.offline, egui::Button::new("重新登录"))
                        .on_hover_text(if self.offline {
                            "离线模式下不需要登录；先去设置里关掉离线"
                        } else {
                            "清掉本地 token 重新走登录流程"
                        })
                        .clicked()
                    {
                        self.logout();
                    }
                    // 本地一个项目都没有时用这份：发给 agent，让它先扫所在仓库、
                    // 把候选项目名列给用户拍板，确认了才建。所以这里不带项目名。
                    if ui
                        .button("复制项目初始化文案")
                        .on_hover_text(
                            "还没有 Teambition 项目时用：把这段发给 agent。它会先在仓库里跑 \
                             tba ensure-project --dry-run，扫出候选项目名（git remote / Cargo.toml / \
                             package.json / 子目录里的项目）连来源一起列给你，你选定之后才建。",
                        )
                        .clicked()
                    {
                        let text = crate::ops::init_prompt(&self.store, None, Lang::Zh);
                        ui.ctx().copy_text(text);
                        self.status = "已复制项目初始化文案：发给 agent，它会先扫仓库列出候选项目名，\
                                       等你拍板后 `tba ensure-project --name <你选的名字>`。"
                            .to_string();
                    }
                    if ui
                        .button("直接接入项目…")
                        .on_hover_text("不用复制文案：选目录 -> 列候选 -> 点一个 -> 直接建好")
                        .clicked()
                    {
                        self.view = View::Create;
                    }
                }
            });
            ui.label(&self.status);
            ui.separator();
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // GitButler / git 状态卡搬去了设置页：它一占就是小半屏，把项目列表整个顶下去。
            // 只在**缺件**时留一行提醒——缺 CLI 时 TBHub 根本不干活，不能让人蒙在鼓里。
            if let Some(msg) = self.git_missing_hint() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!("⚠ {msg}"))
                            .color(egui::Color32::from_rgb(0xE0, 0x7B, 0x39)),
                    );
                    if ui.button("去设置").clicked() {
                        self.view = View::Settings;
                    }
                });
                ui.add_space(4.0);
            }
            if self.view == View::Caps {
                // 能力列表可能很长（尤其字号大时），必须套滚动区，否则底部行被裁掉。
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.draw_caps(ui);
                    });
                return;
            }
            if self.view == View::Settings {
                // 设置页比一屏高（尤其字号调到 22+ 时），不套滚动区的话
                // 下面的「IMAP 主机」和保存/测试按钮会被直接裁掉，看着就像「只有 SMTP」。
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.draw_settings(ui);
                    });
                return;
            }
            if self.view == View::Create {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.draw_create(ui);
                    });
                return;
            }
            if self.needs_token {
                // 登录页比一屏高（字号 18+ 时尤其），不套滚动区底部的「创建步骤」
                // 会被直接裁掉，看着就像「页面只有一半」。
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.draw_setup(ui);
                    });
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.projects.is_empty() && self.loading_projects {
                    ui.label("加载中...");
                    return;
                }
                if self.projects.is_empty() {
                    ui.label("没有项目。点右上角刷新重试。");
                    return;
                }
                // 截图自验用：TBA_UI_ONLY=TBHub 只渲染名字包含它的项目，
                // 一个项目展开就有三块内容，不筛根本截不全。
                let only = std::env::var("TBA_UI_ONLY").unwrap_or_default();
                let projects: Vec<(String, String)> = self
                    .projects
                    .iter()
                    .filter(|(_, n)| only.is_empty() || n.contains(&only))
                    .cloned()
                    .collect();
                let mut to_load: Option<String> = None;
                // Option 四元组: 项目id, 项目名, 复制模式, 文案语言。
                // CopyMode::Queue => 复制「领列1任务」文案（忽略本次任务草稿）；
                // CopyMode::ThisTask => 复制「本次任务」文案（有草稿就带草稿）。
                // Lang::Zh 是准本（内容完整），Lang::En 是压缩英文版（省 token）。
                let mut to_copy: Option<(String, String, CopyMode, Lang)> = None;
                for (pid, pname) in projects {
                    let pid = pid.clone();
                    let pname = pname.clone();
                    let open = self.open.get(&pid).cloned().unwrap_or(self.debug_open);
                    let arrow = if open { "▾" } else { "▸" };
                    let header = ui.selectable_label(false, format!("{arrow} {pname}"));
                    if header.clicked() {
                        let now = !open;
                        self.open.insert(pid.clone(), now);
                    }
                    if open {
                        if !self.skills.contains_key(&pid)
                            && self.loading_skill.as_deref() != Some(pid.as_str())
                        {
                            to_load = Some(pid.clone());
                        }
                        let rows = self.skills.get(&pid).cloned().unwrap_or_default();
                        let loading = self.loading_skill.as_deref() == Some(pid.as_str());
                        // 卡片里点出来的动作，攒到闭包外统一处理（闭包里改 self 会打架）。
                        // (名字, 正文, 是否新建)
                        let mut edit_req: Option<(String, String, bool)> = None;
                        let mut del_req: Option<String> = None;
                        ui.indent(pid.clone(), |ui| {
                            // ---- tb 共享记忆（项目级开关）----
                            //
                            // 开关存 projects/<id>/tb-memory.toml，**不碰 config.toml**
                            // （那里有 token，整体覆写风险太大）。
                            // 记忆正文存在 Teambition「项目记忆 N · 节名」各分片任务的
                            // 富文本自定义字段里：云端、跨设备、网页上直接可看可改。
                            // 分片是必须的——RTF 单字段实测上限 9999 字符。
                            let mut tbm = self.store.load_tb_memory(&pid);
                            let mut toggled = None;
                            ui.horizontal(|ui| {
                                let r = ui.checkbox(&mut tbm.enabled, "启用 tb 共享记忆");
                                if r.changed() {
                                    toggled = Some(tbm.enabled);
                                }
                                ui.label("（存 Teambition 状态为 mem 的任务备注，一条记忆一条任务）");
                            });
                            if let Some(on) = toggled {
                                if let Err(e) = self.store.save_tb_memory(&pid, &tbm) {
                                    self.status = format!("保存开关失败: {e}");
                                } else {
                                    self.status = format!(
                                        "「{}」共享记忆已{}",
                                        pname,
                                        if on { "开启" } else { "关闭" }
                                    );
                                }
                            }
                            ui.horizontal(|ui| {
                                ui.label(format!("记忆状态: {}", tbm.state()));
                                if ui
                                    .add_enabled(!self.mem_busy, egui::Button::new("初始化状态"))
                                    .on_hover_text(
                                        "确保项目里有这个工作流状态（没有就用 taskflow-update 建），幂等",
                                    )
                                    .clicked()
                                {
                                    self.mem_busy = true;
                                    let store = self.store.clone();
                                    let tx = self.tx.clone();
                                    let pid2 = pid.clone();
                                    thread::spawn(move || {
                                        let r = (|| -> Result<String, String> {
                                            let st = ensure_memory_state(&store, &pid2)?;
                                            Ok(format!("状态: {st}\n（已就绪，可以 tba memory --append 加记忆）"))
                                        })();
                                        let _ = tx.send(Ev::MemInit(r));
                                    });
                                }
                                if self.mem_busy {
                                    ui.spinner();
                                }
                            });
                            if tbm.enabled {
                                ui.label(
                                    "注意：建议同时关闭 agent 自带的记忆（如 .workbuddy-ai/memory/MEMORY.md），\
                                     否则两份记忆会打架。tb 不会代你改它们的文件。",
                                );
                            } else {
                                ui.label("关着：tb 不读云端记忆，开工文案里也不加读记忆的指令。");
                            }
                            ui.label(format!(
                                "旧本地记忆目录: {}",
                                self.store.memory_dir(&pid).display()
                            ));
                            ui.add_space(4.0);
                            // 两栏分开、默认收起：tb skill 在云端（按项目），
                            // 通用 skill 在本机（跨项目），混在一个列表里分不清
                            // 「关掉的是这个项目的还是整台机器的」。
                            self.draw_tb_skills(ui, &pid, &rows, loading, &mut edit_req, &mut del_req);
                            self.draw_caps_for_project(ui, &pid);
                            self.draw_compile_section(ui, &pid);
                            ui.add_space(6.0);
                            // 导出：把云端 skill / mem 拉成本地两个 md，人可以直接看
                            // 「agent 读到的到底是什么样」。下载 = 重新拉一遍覆盖本地。
                            ui.horizontal_wrapped(|ui| {
                                let busy = self.skillmem_busy.as_deref() == Some(pid.as_str());
                                if ui
                                    .add_enabled(!busy && !self.offline, egui::Button::new("下载 skill+mem 到本地"))
                                    .on_hover_text(if self.offline {
                                        "离线模式：导出要从 Teambition 拉 skill 和记忆，先关掉离线"
                                    } else {
                                        "从 Teambition 重新拉一次，写成 data/projects/<id>/skillmem/\
                                         skill.md 和 mem.md（多个 skill、多条记忆都合并进去）"
                                    })
                                    .clicked()
                                {
                                    self.skillmem_busy = Some(pid.clone());
                                    let store = self.store.clone();
                                    let tx = self.tx.clone();
                                    let pid2 = pid.clone();
                                    let pname2 = pname.clone();
                                    thread::spawn(move || {
                                        let r = export_skill_mem(&store, &pid2, &pname2).map(|r| {
                                            format!(
                                                "已下载：skill {} 个（启用 {}）、记忆 {} 条 {} 字\n{}\n{}",
                                                r.skill_count,
                                                r.skill_on,
                                                r.mem_count,
                                                r.mem_chars,
                                                r.skill_file.display(),
                                                r.mem_file.display()
                                            )
                                        });
                                        let _ = tx.send(Ev::SkillMem(r));
                                    });
                                }
                                if busy {
                                    ui.spinner();
                                }
                                if ui
                                    .button("打开文件夹")
                                    .on_hover_text("打开 data/projects/<id>/skillmem/")
                                    .clicked()
                                {
                                    let dir = self.store.skillmem_dir(&pid);
                                    self.status = match open_folder(&dir) {
                                        Ok(()) => format!("已打开 {}", dir.display()),
                                        Err(e) => e,
                                    };
                                }
                            });
                            ui.label(format!(
                                "本地导出目录: {}",
                                self.store.skillmem_dir(&pid).display()
                            ));
                            ui.add_space(6.0);
                            ui.label("本次任务（留空 = 让 agent 去队列「未完成」领）:");
                            {
                                let draft = self.task_drafts.entry(pid.clone()).or_default();
                                ui.add(
                                    egui::TextEdit::multiline(draft)
                                        .desired_rows(3)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("首行写 `# 标题`，下面写正文。填了就在列「进行中」新建一条任务"),
                                );
                            }
                            ui.add_space(4.0);
                            // 两行四按钮：模式（本次任务 / 领列1）× 语言（中文准本 / 英文省 token）。
                            // 中文是准本，英文由中文压缩而来；两份规则一致，只是长度不同。
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("复制文案(本次任务·中文)").clicked() {
                                    to_copy = Some((
                                        pid.clone(),
                                        pname.clone(),
                                        CopyMode::ThisTask,
                                        Lang::Zh,
                                    ));
                                }
                                if ui.button("复制文案(本次任务·英文)").clicked() {
                                    to_copy = Some((
                                        pid.clone(),
                                        pname.clone(),
                                        CopyMode::ThisTask,
                                        Lang::En,
                                    ));
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("复制文案(领列1·中文)").clicked() {
                                    to_copy = Some((
                                        pid.clone(),
                                        pname.clone(),
                                        CopyMode::Queue,
                                        Lang::Zh,
                                    ));
                                }
                                if ui.button("复制文案(领列1·英文)").clicked() {
                                    to_copy = Some((
                                        pid.clone(),
                                        pname.clone(),
                                        CopyMode::Queue,
                                        Lang::En,
                                    ));
                                }
                            });
                        });
                        // 编辑 / 删除攒到这里再动 self：在卡片闭包里改会和 ui 的借用打架。
                        if let Some((name, content, is_new)) = edit_req {
                            self.skill_edit = Some(SkillEdit {
                                project_id: pid.clone(),
                                project_name: pname.clone(),
                                name,
                                content,
                                is_new,
                            });
                        }
                        if let Some(name) = del_req {
                            self.skill_saving = true;
                            let store = self.store.clone();
                            let tx = self.tx.clone();
                            let pid2 = pid.clone();
                            thread::spawn(move || {
                                let r = archive_skill(&store, &pid2, &name);
                                let _ = tx.send(Ev::SkillSaved(pid2, r));
                            });
                        }
                    }
                    ui.separator();
                }
                if let Some(pid) = to_load {
                    self.load_skills(&pid);
                }
                if let Some((pid, pname, mode, lang)) = to_copy {
                    let (text, has_task) = self.copy_prompt(&pid, &pname, mode, lang);
                    ui.ctx().copy_text(text);
                    self.status = format!(
                        "已复制 Agent 文案（{}，{}）。",
                        lang.label(),
                        if has_task {
                            "含本次任务，agent 会新建任务开工"
                        } else {
                            "agent 会去队列「未完成」领任务"
                        }
                    );
                }
            });
        });

        // 编辑窗画在最上层，不受侧栏/中央面板影响。
        self.draw_skill_editor(ctx);
        self.draw_cap_editor(ctx);
        self.draw_cap_delete(ctx);
        self.draw_text_view(ctx);
    }
}

impl App {
    /// 只读文本查看窗（独立系统窗口）。目前给「编译 & 重载 → 查看文案」用。
    fn draw_text_view(&mut self, ctx: &egui::Context) {
        let Some((title, text)) = self.text_view.clone() else {
            return;
        };
        let mut close = false;
        let mut copy = false;
        let (close2, copy2) = show_editor_window(
            ctx,
            "tba-text-view",
            title,
            [820.0, 620.0],
            [520.0, 360.0],
            |ui| {
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    close = true;
                }
                // 按钮放在上面：ScrollArea 用 auto_shrink=false 会吃满剩余高度，
                // 放它下面就会被挤出窗口（实测截图里根本看不见）。
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(
                            "这就是开工文案里 agent 实际收到的那一段，不是另写的说明。",
                        )
                        .small()
                        .weak(),
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.button("关闭").clicked() {
                                close = true;
                            }
                            if ui.button("复制").clicked() {
                                copy = true;
                            }
                        },
                    );
                });
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&text).monospace().size(13.0),
                            )
                            .wrap(),
                        );
                    });
                (close, copy)
            },
        );
        if copy2 {
            ctx.copy_text(text.clone());
        }
        if close || close2 {
            self.text_view = None;
        }
    }
    // 通用能力页：本地、跨项目的文本能力库。
    //
    // 和「项目列表」里的 skill 是两套东西：项目 skill 存 Teambition 云端、按项目隔离，
    // 换台设备/换个项目就没了；能力是这台机器上的 tba 能干什么，存 data/caps.toml，
    // 所有项目共用，也正因为如此才需要在这里直接改，而不是每次都发版改代码。
    fn draw_caps(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("能力");
            if ui.button("新增").on_hover_text("新建一条本地能力").clicked() {
                self.cap_edit = Some(CapEdit::new());
            }
            if ui
                .button("恢复内置")
                .on_hover_text("补齐缺失的内置能力；已存在的只把正文拉回默认，开关不动")
                .clicked()
            {
                self.status = match caps::restore(&self.store, None) {
                    Ok(s) => s,
                    Err(e) => format!("恢复失败: {e}"),
                };
                self.caps = caps::list(&self.store);
            }
            if ui.button("打开数据目录").clicked() {
                if let Err(e) = crate::ops::open_folder(&self.store.root) {
                    self.status = format!("打开目录失败: {e}");
                }
            }
        });
        ui.label(
            "本地、跨项目：不跟着 Teambition 项目走，换项目也还在。agent 用 `tba cap list` \
             看目录、`tba cap <名字>` 读正文；关掉的能力不会进 agent 文案。",
        );
        ui.add_space(6.0);

        let total = self.caps.len();
        let on = self.caps.iter().filter(|c| c.on).count();
        ui.label(egui::RichText::new(format!("{total} 条，启用 {on} 条")).weak());
        ui.separator();

        if total == 0 {
            ui.label("还没有能力。点「新增」写一条，或「恢复内置」找回默认的几条。");
            return;
        }

        // 行内动作攒到循环外统一处理：闭包里同时改 self 会打架。
        let mut toggle: Option<(String, bool)> = None;
        let mut edit: Option<Cap> = None;
        let mut del: Option<String> = None;
        for c in self.caps.clone() {
            ui.horizontal(|ui| {
                let mut is_on = c.on;
                if ui
                    .checkbox(&mut is_on, "")
                    .on_hover_text("开关：关掉后不进 agent 文案，但正文还在")
                    .changed()
                {
                    toggle = Some((c.name.clone(), is_on));
                }
                ui.label(egui::RichText::new(&c.name).monospace().strong());
                if c.builtin {
                    ui.label(egui::RichText::new("内置").small().weak());
                }
                if c.always {
                    ui.label(
                        egui::RichText::new("常驻")
                            .small()
                            .color(egui::Color32::from_rgb(200, 140, 40)),
                    )
                    .on_hover_text("开工文案里会标出来，给「动手前就得知道」的红线用");
                }
                ui.label(egui::RichText::new(&c.summary).weak());
                // 按钮靠右、尺寸统一：先加的在右边，所以先加「删除」再加「编辑」。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("删除").clicked() {
                        del = Some(c.name.clone());
                    }
                    if ui.button("编辑").clicked() {
                        edit = Some(Cap::clone(&c));
                    }
                });
            });
            ui.separator();
        }

        if let Some((n, v)) = toggle {
            match caps::set_on(&self.store, &n, v) {
                Ok(m) => self.status = m,
                Err(e) => self.status = format!("改开关失败: {e}"),
            }
            self.caps = caps::list(&self.store);
        }
        if let Some(c) = edit {
            self.cap_edit = Some(CapEdit::from(&c));
        }
        if let Some(n) = del {
            self.cap_del = Some(n);
        }
    }

    // 能力编辑窗。新增和编辑共用：名字在编辑时锁住（改名等于换键，删了重建更清楚）。
    fn draw_cap_editor(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.cap_edit.clone() else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        // 独立窗口：以前画在主窗口里，主窗一小就被裁掉，切页面还会被盖住。
        let title = if ed.is_new {
            "新增能力 · TBA".to_string()
        } else {
            format!("编辑能力 · {} · TBA", ed.name)
        };
        let (save, cancel) = show_editor_window(
            ctx,
            "tba-cap-editor",
            title,
            [880.0, 760.0],
            [620.0, 480.0],
            |ui| {
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    cancel = true;
                }
                ui.label("名字（agent 用它 `tba cap <名字>` 取正文；只能用字母、数字、- 和 _）:");
                ui.add_enabled(
                    ed.is_new,
                    egui::TextEdit::singleline(&mut ed.name)
                        .desired_width(f32::INFINITY)
                        .hint_text("例如 deploy-web"),
                );
                if !ed.is_new {
                    ui.label(
                        egui::RichText::new("编辑时不能改名；要改名就删了重建。")
                            .small()
                            .weak(),
                    );
                }
                ui.add_space(6.0);
                ui.label("一句话说明（写「什么时候用」，agent 靠这一行决定要不要读正文）:");
                ui.add(
                    egui::TextEdit::singleline(&mut ed.summary)
                        .desired_width(f32::INFINITY)
                        .hint_text("例如 要部署：一条命令打包并上传"),
                );
                ui.add_space(6.0);
                ui.label("英文说明（可选：英文开工文案用，空着就用上面那句）:");
                ui.add(
                    egui::TextEdit::singleline(&mut ed.summary_en)
                        .desired_width(f32::INFINITY)
                        .hint_text("e.g. to deploy: one command to build and upload"),
                );
                ui.add_space(6.0);
                ui.checkbox(&mut ed.always, "常驻：开工文案里标出来，先被看见")
                    .on_hover_text(
                        "只给「动手前就得知道、等踩了坑再读已经晚了」的红线用（比如禁止弹命令行）。\
                         其余能力都是索引 + 按需读正文；勾了会让每次开工的文案变长。",
                    );
                ui.add_space(6.0);
                ui.label("正文（markdown：什么时候用、参数、红线。实现留在命令里）:");
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut ed.body)
                                .desired_rows(18)
                                .desired_width(f32::INFINITY)
                                .code_editor(),
                        );
                    });
                if !ed.err.is_empty() {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(&ed.err).color(egui::Color32::RED));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let ok = !ed.name.trim().is_empty();
                    if ui
                        .add_enabled(ok, egui::Button::new("保存"))
                        .on_hover_text("写到 data/caps.toml，立刻生效（agent 下次读就是新正文）")
                        .clicked()
                    {
                        save = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                    if ed.builtin {
                        ui.label(
                            egui::RichText::new("内置条目：改坏了可以「恢复内置」")
                                .small()
                                .weak(),
                        );
                    }
                });
                (save, cancel)
            },
        );

        if cancel {
            // 不再画这个 viewport，eframe 下一帧会把它连同窗口一起收掉。
            self.cap_edit = None;
            return;
        }
        // 打字中间态要留住，否则每帧被上一帧覆盖。
        self.cap_edit = Some(ed.clone());
        if !save {
            return;
        }
        let name = ed.name.trim().to_string();
        if let Err(e) = caps::check_name(&name) {
            ed.err = e;
            self.cap_edit = Some(ed);
            return;
        }
        // 新增撞名直接拒绝：覆盖内置条目是最容易后悔的操作，要改请点那一条的「编辑」。
        if ed.is_new && caps::get(&self.store, &name).is_some() {
            ed.err = format!("已有同名能力「{name}」，改个名字，或直接编辑那一条。");
            self.cap_edit = Some(ed);
            return;
        }
        let cap = Cap {
            name,
            summary: ed.summary.trim().to_string(),
            summary_en: ed.summary_en.trim().to_string(),
            on: ed.on,
            builtin: ed.builtin,
            always: ed.always,
            body: ed.body.clone(),
        };
        match caps::upsert(&self.store, cap) {
            Ok(m) => {
                self.status = m;
                self.caps = caps::list(&self.store);
                self.cap_edit = None;
            }
            Err(e) => {
                ed.err = e;
                self.cap_edit = Some(ed);
            }
        }
    }

    // 删除前的二次确认。内置条目能恢复，自定义条目删了就是真没了。
    fn draw_cap_delete(&mut self, ctx: &egui::Context) {
        let Some(name) = self.cap_del.clone() else {
            return;
        };
        let builtin = self.caps.iter().any(|c| c.name == name && c.builtin);
        let mut open = true;
        let mut yes = false;
        let mut cancel = false;
        egui::Window::new("删除能力")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("确定删除「{name}」？"));
                ui.label(if builtin {
                    "这是内置条目，删了可以在「能力」页点「恢复内置」找回。"
                } else {
                    "自定义条目删了就没了，要留就先复制正文备份。"
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("删除").clicked() {
                        yes = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if yes {
            self.status = match caps::remove(&self.store, &name) {
                Ok(m) => m,
                Err(e) => format!("删除失败: {e}"),
            };
            self.caps = caps::list(&self.store);
            self.cap_del = None;
        } else if cancel || !open {
            self.cap_del = None;
        }
    }

    // skill 编辑窗。新建和编辑共用一套：is_new 只影响标题文字。
    // 保存走 push_skill（同名则更新），删除走 archive_skill。
    fn draw_skill_editor(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.skill_edit.clone() else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        // 同能力编辑窗：独立窗口，免得被主窗裁掉或盖住。
        let title = if ed.is_new {
            format!("新建 skill · {} · TBA", ed.project_name)
        } else {
            format!("编辑 skill · {} · TBA", ed.name)
        };
        let (save, cancel) = show_editor_window(
            ctx,
            "tba-skill-editor",
            title,
            [760.0, 700.0],
            [540.0, 440.0],
            |ui| {
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    cancel = true;
                }
            ui.label(format!("项目: {}", ed.project_name));
            ui.add_space(4.0);
            ui.label("名字（agent 用它来 `tba skill <名字>` 取正文，短一点省 token）:");
            ui.add(
                egui::TextEdit::singleline(&mut ed.name)
                    .desired_width(f32::INFINITY)
                    .hint_text("例如 release-push"),
            );
            ui.add_space(6.0);
            ui.label("正文（markdown，写给 agent 看的操作步骤）:");
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut ed.content)
                            .desired_rows(14)
                            .desired_width(f32::INFINITY)
                            .code_editor(),
                    );
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok = !ed.name.trim().is_empty() && !self.skill_saving;
                if ui
                    .add_enabled(ok, egui::Button::new("保存"))
                    .on_hover_text("写进 Teambition 的 skill 状态任务；同名会覆盖旧正文")
                    .clicked()
                {
                    save = true;
                }
                if ui.button("取消").clicked() {
                    cancel = true;
                }
                if self.skill_saving {
                    ui.spinner();
                    ui.label("正在写 Teambition...");
                }
            });
                (save, cancel)
            },
        );

        if cancel {
            self.skill_edit = None;
            return;
        }
        // 用户正在打字的中间态要留住，否则每帧都被上一帧覆盖。
        self.skill_edit = Some(ed.clone());
        if save {
            let name = ed.name.trim().to_string();
            let content = ed.content;
            let pid = ed.project_id;
            self.skill_saving = true;
            let store = self.store.clone();
            let tx = self.tx.clone();
            thread::spawn(move || {
                let r = push_skill(&store, &pid, &name, &content);
                let _ = tx.send(Ev::SkillSaved(pid, r));
            });
        }
    }

    // 四个「复制 agent 文案」按钮共用的动作。
    //
    // 抽成方法是为了能直接测：按钮本身只负责把返回值塞进剪贴板，
    // 「本次任务 vs 领列1任务」的行为差异全在这里，一处说了算；
    // 语言差异（中文准本 / 英文压缩版）由 agent_prompt_from 按 lang 选，UI 不重复。
    //
    // 返回 (文案, 是否带上了「本次任务」)，第二个值用来决定底部状态提示。
    fn copy_prompt(
        &mut self,
        pid: &str,
        pname: &str,
        mode: CopyMode,
        lang: Lang,
    ) -> (String, bool) {
        let rows = self.skills.get(pid).cloned().unwrap_or_default();
        // 草稿的组装交给 draft_for_mode：Queue 模式强制丢弃界面里的草稿，
        // ThisTask 模式按草稿是否填写决定带不带。UI 不再重复这段逻辑。
        let raw = self.task_drafts.get(pid).cloned().unwrap_or_default();
        let draft = draft_for_mode(&self.store, &raw, mode);
        let has_task = draft.is_some();
        let text = agent_prompt_from(&self.store, pid, pname, &rows, draft.as_ref(), lang);
        (text, has_task)
    }

    // 通用能力：重载。编译 -> 杀旧 -> 复制校验 -> 启动。
    // ---- 项目卡片里的三块：tb skill / 通用 skill / 编译 & 重载 ----
    //
    // 为什么都要默认收起：一个项目展开就有三个列表 + 一堆按钮，找「复制文案」
    // 得往下滚半天。默认收起后，卡片高度只跟项目数有关，扫一眼就能定位。
    fn draw_tb_skills(
        &mut self,
        ui: &mut egui::Ui,
        pid: &str,
        rows: &[SkillItem],
        loading: bool,
        edit_req: &mut Option<(String, String, bool)>,
        del_req: &mut Option<String>,
    ) {
        let on_n = rows.iter().filter(|r| r.on).count();
        let title = if loading {
            "tb skill（加载中…）".to_string()
        } else {
            format!("tb skill（{} 个，{} 开）", rows.len(), on_n)
        };
        egui::CollapsingHeader::new(title)
            .id_salt(("tb-skill", pid))
            .default_open(self.debug_open)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(if self.offline {
                        "离线模式：这一份来自本地缓存（data/projects/<id>/skills.json）。\
                         勾选仍然生效；编辑 / 删除要联网，已置灰。"
                    } else {
                        "存 Teambition 云端、按项目隔离。勾选 = 这个项目启用；编辑/删除都改云端那条任务。"
                    })
                    .small()
                    .weak(),
                );
                if loading {
                    ui.label("正在加载…");
                } else if rows.is_empty() {
                    ui.label(if self.offline {
                        "离线模式：这个项目还没有能力缓存（联网打开一次就会存下来）。"
                    } else {
                        "没有 tb skill。"
                    });
                }
                for row in rows {
                    let mut on = row.on;
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut on, &row.name).changed() {
                            let _ = self.store.set_switch(pid, &row.name, on);
                            if let Some(list) = self.skills.get_mut(pid) {
                                if let Some(r) = list.iter_mut().find(|r| r.name == row.name) {
                                    r.on = on;
                                }
                            }
                            self.status =
                                format!("skill「{}」已{}", row.name, if on { "开启" } else { "关闭" });
                        }
                        // 编辑 / 删除都走后台线程：要联网写 Teambition。离线时置灰，
                        // 点了也只会报错，不如直接不给点。
                        if ui
                            .add_enabled(!self.offline, egui::Button::new("编辑"))
                            .on_hover_text(if self.offline {
                                "离线模式：改云端 skill 要联网"
                            } else {
                                "改正文并写回 Teambition"
                            })
                            .clicked()
                        {
                            *edit_req = Some((row.name.clone(), row.content.clone(), false));
                        }
                        if ui
                            .add_enabled(!self.offline, egui::Button::new("删除"))
                            .clicked()
                        {
                            *del_req = Some(row.name.clone());
                        }
                    });
                }
                if ui
                    .add_enabled(!self.offline, egui::Button::new("+ 新建 skill"))
                    .on_hover_text(if self.offline {
                        "离线模式：新建 skill 要写 Teambition，先关掉离线"
                    } else {
                        "写一段 markdown 存到 Teambition 的 skill 状态任务里。\
                         agent 用 `tba skills` 看到名字，用到才读正文（省 token）"
                    })
                    .clicked()
                {
                    *edit_req = Some((String::new(), String::new(), true));
                }
            });
    }

    // 通用 skill：本地、跨项目的能力，这里只给开关。
    // 勾选 = 这个项目用；取消 = 这个项目不用，agent 文案里也不会出现（也就搜不到）。
    // 不提供编辑/删除：那是「能力」页的事，这里改的是「这个项目用不用」。
    fn draw_caps_for_project(&mut self, ui: &mut egui::Ui, pid: &str) {
        let caps = self.caps.clone();
        if !self.cap_off.contains_key(pid) {
            self.cap_off
                .insert(pid.to_string(), self.store.load_cap_off(pid));
        }
        let off = self.cap_off.get(pid).cloned().unwrap_or_default();
        let on_n = caps
            .iter()
            .filter(|c| c.on && !off.iter().any(|n| n == &c.name))
            .count();
        egui::CollapsingHeader::new(format!("通用 skill（{} 条，{} 开）", caps.len(), on_n))
            .id_salt(("caps", pid))
            .default_open(self.debug_open)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "本机、跨项目的能力。勾选 = 这个项目用；取消 = 这个项目不用，\
                         agent 文案里不会出现。要改正文去「通用能力 → 能力」。",
                    )
                    .small()
                    .weak(),
                );
                for c in &caps {
                    let mut on = c.on && !off.iter().any(|n| n == &c.name);
                    let label = format!("{} — {}", c.name, c.summary);
                    if !c.on {
                        // 全局关掉的，项目里勾也没意义：显示成禁用 + 说明。
                        ui.add_enabled(false, egui::Checkbox::new(&mut on, label));
                        ui.label(egui::RichText::new("（全局已关）").small().weak());
                        continue;
                    }
                    if ui.checkbox(&mut on, label).changed() {
                        match self.store.set_cap_off(pid, &c.name, !on) {
                            Ok(()) => {
                                self.cap_off
                                    .insert(pid.to_string(), self.store.load_cap_off(pid));
                                self.status = format!(
                                    "「{}」在本项目已{}",
                                    c.name,
                                    if on { "启用" } else { "停用（agent 搜不到）" }
                                );
                            }
                            Err(e) => self.status = format!("保存失败: {e}"),
                        }
                    }
                }
            });
    }

    // 编译 & 重载：参数存 Teambition 状态为 setting 的任务（标题「项目设置」），
    // 本地留一份缓存好秒开。第一次展开时后台拉一次云端，之后用内存里的编辑态。
    fn draw_compile_section(&mut self, ui: &mut egui::Ui, pid: &str) {
        if !self.setting.contains_key(pid) {
            let cached = crate::setting::ProjectSetting::load_cached(&self.store, pid)
                .unwrap_or_default();
            self.setting.insert(pid.to_string(), cached);
        }
        let mut ed = self.setting.get(pid).cloned().unwrap_or_default();
        let busy = self.reload_busy.as_deref() == Some(pid);
        // 文本缓冲：第一次进来用当前设置渲染成 TOML，之后就是用户手里的那份。
        if !self.compile_text.contains_key(pid) {
            self.compile_text.insert(pid.to_string(), ed.to_note());
        }
        // 标题按文本里解析出来的 on 走，改了 `on = false` 立刻反映到折叠标题上。
        let parsed_now = crate::setting::ProjectSetting::parse_note(
            self.compile_text.get(pid).map(|s| s.as_str()).unwrap_or(""),
        );
        let title = if parsed_now.on {
            "编译 & 重载".to_string()
        } else {
            "编译 & 重载（已停用）".to_string()
        };
        let mut save = false;
        let mut refetch = false;
        let mut detect = false;
        let mut action: Option<&'static str> = None;

        egui::CollapsingHeader::new(title)
            .id_salt(("compile", pid))
            .default_open(self.debug_open)
            .show(ui, |ui| {
                // 展开过就拉一次云端：本地缓存可能是上次的，编译指令改了要能看到。
                if self.setting_fetched.get(pid) != Some(&true) && self.setting_busy.is_none() {
                    self.setting_fetched.insert(pid.to_string(), true);
                    self.setting_busy = Some(pid.to_string());
                    let store = self.store.clone();
                    let tx = self.tx.clone();
                    let pid2 = pid.to_string();
                    thread::spawn(move || {
                        let r = crate::setting::fetch(&store, &pid2);
                        let _ = tx.send(Ev::SettingLoaded(pid2, r));
                    });
                }
                ui.label(
                    egui::RichText::new(
                        "参数存 Teambition：状态 setting 的任务「项目设置」。下面就是那段文本，\
                         网页上看到的也是同一份——改完点「保存到 Teambition」。",
                    )
                    .small()
                    .weak(),
                );
                ui.label(
                    egui::RichText::new(
                        "字段：on(true/false) · compile · root · name(exe 基名，不带 .exe) · \
                         mode(rename 改名直跑 / copy 副本运行) · live(copy 模式的副本路径) · args(启动参数)",
                    )
                    .small()
                    .weak(),
                );
                // 文本就是真相：这段 TOML 原样存进 Teambition 任务备注。
                // 之前是六个输入框，改一个参数得在格子里挨个找，还看不全整体。
                let mut text = self.compile_text.get(pid).cloned().unwrap_or_default();
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .code_editor()
                        .desired_rows(9)
                        .desired_width(f32::INFINITY)
                        .hint_text(
                            "# 一行一个字段，例如：\n\
                             # on = true\n\
                             # compile = \"cargo build --release\"\n\
                             # root = 'D:\\dev\\mcp'\n\
                             # name = \"tba\"\n\
                             # mode = \"rename\"\n\
                             # args = \"ui\"",
                        ),
                );
                if resp.changed() {
                    self.compile_text.insert(pid.to_string(), text.clone());
                }
                // 解析结果就地反馈：语法对不对、有没有明显会跑挂的。
                // 解析不了也照样能用（逐行扫 key = value），所以只是提示不是错误。
                let parsed = crate::setting::ProjectSetting::parse_note(&text);
                let mut warns: Vec<String> = Vec::new();
                let mut hints: Vec<String> = Vec::new();
                if !crate::setting::ProjectSetting::is_toml(&text) {
                    warns.push(
                        "不是合法 TOML（多半是 Windows 路径里的 \\ 或引号写错），已按行读取：\
                         字段照样能认，但建议路径用单引号 'D:\\dev\\mcp'"
                            .to_string(),
                    );
                }
                warns.extend(parsed.problems());
                if parsed.compile.trim().is_empty() {
                    hints.push(
                        "compile 空着 = 还没定：可以点「探测默认值」，或让 agent 跑 \
                         `tba setting guess`。空着仍可「只重启」。"
                            .to_string(),
                    );
                }
                if warns.is_empty() && hints.is_empty() {
                    ui.label(egui::RichText::new("✓ 解析正常").small().weak());
                }
                for w in warns {
                    ui.label(
                        egui::RichText::new(format!("⚠ {w}"))
                            .small()
                            .color(egui::Color32::from_rgb(0xE0, 0x7B, 0x39)),
                    );
                }
                for h in hints {
                    ui.label(egui::RichText::new(format!("· {h}")).small().weak());
                }

                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.setting_busy.as_deref() != Some(pid),
                            egui::Button::new(if self.offline {
                                "保存（离线·存本地）"
                            } else {
                                "保存到 Teambition"
                            }),
                        )
                        .on_hover_text(if self.offline {
                            "离线模式：写本地缓存 data/projects/<id>/setting.toml，不碰云端"
                        } else {
                            "写进状态 setting 的任务备注，换设备也还在"
                        })
                        .clicked()
                    {
                        save = true;
                    }
                    if ui
                        .button("探测默认值")
                        .on_hover_text(
                            "按项目现状补 root / name / compile，**只填空着的**，填过的一律不动。\
                             补完直接显示在文本里，还要点保存才写回云端",
                        )
                        .clicked()
                    {
                        detect = true;
                    }
                    if ui
                        .add_enabled(
                            self.setting_busy.as_deref() != Some(pid),
                            egui::Button::new(if self.offline {
                                "重读本地"
                            } else {
                                "从云端重读"
                            }),
                        )
                        .clicked()
                    {
                        refetch = true;
                    }
                    // 文案就是开工时给 agent 的那一段（和 `tba setting text` 同源）。
                    // 按当前文本算：改完没保存也能先看效果，保存后 agent 才真收到。
                    if ui
                        .button("查看文案")
                        .on_hover_text(
                            "看开工文案里「编译 & 重载」那一段（按当前文本内容生成；\
                             保存到 Teambition 后 agent 才会收到）",
                        )
                        .clicked()
                    {
                        let exe = crate::ops::prompt_exe(&self.store);
                        let block = parsed.prompt_block(&exe, true);
                        let text = if block.trim().is_empty() {
                            "「编译 & 重载」已停用：这段不会出现在开工文案里。".to_string()
                        } else {
                            block.trim_end().to_string()
                        };
                        self.text_view = Some(("编译 & 重载 · 开工文案".to_string(), text));
                    }
                    if self.setting_busy.as_deref() == Some(pid) {
                        ui.spinner();
                    }
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("编译 & 重载"))
                        .on_hover_text("编译 -> 杀旧 -> 复制校验 -> 启动")
                        .clicked()
                    {
                        action = Some("build");
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("只重启"))
                        .on_hover_text("不编译，直接杀旧 + 启动")
                        .clicked()
                    {
                        action = Some("restart");
                    }
                    if ui.add_enabled(!busy, egui::Button::new("杀掉")).clicked() {
                        action = Some("kill");
                    }
                    if ui.add_enabled(!busy, egui::Button::new("状态")).clicked() {
                        action = Some("status");
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                if let Some(out) = self.reload_out.get(pid) {
                    ui.label(egui::RichText::new(out).monospace());
                }
                if let Some(log) = self.reload_log.get(pid) {
                    if !log.is_empty() {
                        ui.collapsing("reload.log 末尾", |ui| {
                            egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                                ui.add(egui::Label::new(
                                    egui::RichText::new(log).monospace().small(),
                                ));
                            });
                        });
                    }
                }
            });

        // 文本 -> 结构体：保存、跑「编译 & 重载」都用解析出来的值，
        // 这样「文本里写的就是会执行的东西」，不会出现表单和文本两套真相。
        let text_now = self.compile_text.get(pid).cloned().unwrap_or_default();
        ed = crate::setting::ProjectSetting::parse_note(&text_now);
        if detect {
            // 探测默认值：只补空着的字段，然后把结果写回文本让人过目。
            let mut st = ed.clone();
            st.fill_defaults();
            self.compile_text.insert(pid.to_string(), st.to_note());
            ed = st;
        }
        // 编辑态写回：CollapsingHeader 收起时闭包不执行，这里只在展开时覆盖，
        // 不会把云端刚拉回来的值冲掉（拉回来时闭包还没跑）。
        self.setting.insert(pid.to_string(), ed.clone());
        if refetch {
            self.setting_fetched.insert(pid.to_string(), false);
            self.setting_busy = Some(pid.to_string());
            let store = self.store.clone();
            let tx = self.tx.clone();
            let pid2 = pid.to_string();
            thread::spawn(move || {
                let r = crate::setting::fetch(&store, &pid2);
                let _ = tx.send(Ev::SettingLoaded(pid2, r));
            });
        }
        if save {
            self.setting_busy = Some(pid.to_string());
            let store = self.store.clone();
            let tx = self.tx.clone();
            let pid2 = pid.to_string();
            let mut st = ed.clone();
            // 空字段按项目现状补齐再存，否则云端会留下一个「有任务没参数」的设置。
            st.fill_defaults();
            // 文本同步成规范化后的 TOML：手写的引号/转义在这里被修正，
            // 下次打开看到的就是实际存下去的东西。
            self.compile_text.insert(pid.to_string(), st.to_note());
            self.setting.insert(pid.to_string(), st.clone());
            thread::spawn(move || {
                let r = crate::setting::save(&store, &pid2, &st);
                let _ = tx.send(Ev::SettingSaved(pid2, r));
            });
        }
        if let Some(a) = action {
            self.start_reload(pid, a);
        }
    }

    // 跑一次编译 & 重载。参数就是上面那张表里的值——存没存过都能跑，
    // 存过只是让它换设备还在。
    fn start_reload(&mut self, pid: &str, action: &str) {
        let st = self.setting.get(pid).cloned().unwrap_or_default();
        self.reload_busy = Some(pid.to_string());
        self.reload_out
            .insert(pid.to_string(), format!("正在执行 {action}..."));
        let req = crate::capability::ReloadReq {
            action: action.to_string(),
            root: st.root.clone(),
            name: st.name.clone(),
            live: st.live.clone(),
            args: st.args.clone(),
            mode: st.mode.clone(),
            compile: st.compile.clone(),
            elevate: false,
            release_src: false,
        };
        let pid2 = pid.to_string();
        let tx = self.tx.clone();
        // 工具链路径要从配置里读，所以 store 得跟着进线程。
        let store = self.store.clone();
        thread::spawn(move || {
            let mut req = req;
            // 自己重载自己：先把 cargo 产物重命名挪开，链接器才写得进去
            req.release_src =
                req.action == "build" && crate::capability::is_src(&req.root, &req.name);
            let self_src = req.release_src;
            match crate::capability::run_sync(&req, &store) {
                Ok(r) => {
                    let ok = r.code == 0;
                    let text = if ok {
                        r.out
                    } else if !r.err.is_empty() {
                        r.err
                    } else {
                        format!("exit {}", r.code)
                    };
                    let _ = tx.send(Ev::ReloadDone(pid2, text));
                    // 重载 tba 自己：新实例已经起来，旧窗口让位
                    if ok && self_src {
                        thread::sleep(std::time::Duration::from_millis(1200));
                        std::process::exit(0);
                    }
                }
                Err(e) => {
                    let _ = tx.send(Ev::ReloadDone(pid2, format!("ERR run:{e}")));
                }
            }
        });
    }

    // 没有 token 时的引导面板：打开网页 -> 粘贴 -> 保存。
    fn draw_setup(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            ui.heading("还没有 Teambition 访问令牌");
            ui.add_space(10.0);
            ui.label("tba 通过 Teambition MCP 读写项目、任务和 skill。");
            ui.label("需要先在 Teambition 上创建 User Token，再粘贴到下面。");
            ui.add_space(24.0);

            if ui
                .button("① 打开 Teambition 创建 Token")
                .on_hover_text(TOKEN_PAGE)
                .clicked()
            {
                crate::oauth::open_browser(TOKEN_PAGE);
            }
            ui.add_space(4.0);
            ui.hyperlink_to(TOKEN_PAGE, TOKEN_PAGE);
            ui.add_space(16.0);

            ui.label("② 把「查看详情」里复制的 Token ID 粘贴到这里:");
            ui.add_space(4.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.token_input)
                    .hint_text("粘贴 Token ID")
                    .desired_width(440.0),
            );
            if self.token_input.is_empty() {
                resp.request_focus();
            }
            ui.add_space(14.0);

            let can_save = !self.token_input.trim().is_empty() && !self.setup_busy;
            if ui
                .add_enabled(can_save, egui::Button::new("③ 保存并连接"))
                .clicked()
            {
                let t = self.token_input.trim().to_string();
                self.save_token(t);
            }
            if self.setup_busy {
                ui.add_space(8.0);
                ui.spinner();
            }
            ui.add_space(24.0);
            ui.separator();
            ui.add_space(10.0);
            ui.label("创建步骤:");
            ui.label("  1. 用钉钉扫码登录 Teambition");
            ui.label("  2. 点「创建 Token」按钮");
            ui.label("  3. 填 Token 名称、有效期(建议 1 年)、选择使用企业");
            ui.label("  4. 点确认，再点「查看详情」复制 Token ID");
            ui.add_space(8.0);
            ui.label("也可以命令行操作: tb login --token <令牌>");
        });
    }

    // 设置：字体大小等界面偏好。改动即时生效（update 里按 applied_font 差异重设样式）
    // 并写入 data/ui.toml，重启窗口也会保留。
    // 构建工具链（MSVC + Windows SDK）。
    //
    // 为什么要有这块：编译 & 重载要靠 link.exe 和 SDK 的库，而每台机器的 VS
    // 版本、SDK 位置都不一样（有的还在别的盘）。自动探测只覆盖标准位置，
    // 探不到时让用户点三次「选择」，比让人去翻文档快得多。
    fn draw_toolchain(&mut self, ui: &mut egui::Ui) {
        use crate::toolchain as tc;
        if self.tc.is_none() {
            self.tc = Some(tc::resolve(&self.store));
        }
        let ready = self.tc.as_ref().map(|r| r.ready()).unwrap_or(false);
        let mut pick: Option<u8> = None;
        let mut do_guess = false;
        let mut do_clear = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("构建工具链");
                if ready {
                    ui.colored_label(egui::Color32::from_rgb(0x2e, 0x7d, 0x32), "✓ 可用");
                } else {
                    ui.colored_label(egui::Color32::from_rgb(0xc6, 0x28, 0x28), "✗ 不可用");
                }
                if let Some(r) = &self.tc {
                    ui.label(format!("来源: {}", r.source.label()));
                }
            });
            ui.label(
                egui::RichText::new(
                    "编译 & 重载要用 MSVC 的 link.exe 和 Windows SDK 的库。只认 MSVC——\
                     发布包就是它编的，GNU 编出来的东西行为不一样。",
                )
                .small()
                .weak(),
            );
            match &self.tc {
                Some(r) if r.ready() => {
                    ui.label(egui::RichText::new(format!("link.exe: {}", r.tc.msvc_bin)).small());
                    ui.label(egui::RichText::new(format!("SDK lib : {}", r.tc.sdk_lib)).small());
                }
                Some(r) => {
                    for p in &r.problems {
                        ui.colored_label(
                            egui::Color32::from_rgb(0xc6, 0x28, 0x28),
                            format!("· {p}"),
                        );
                    }
                    ui.label(
                        egui::RichText::new(
                            "点下面三个「选择」指到对应目录（里面有那个文件就行）；\
                             选完立刻验活，路径不对不会保存。",
                        )
                        .small(),
                    );
                }
                None => {}
            }
            // 手动安装入口**常显**：不管当前探没探到，这两个下载地址都在。
            // 装好了也可能是别人装的旧版、或者要装到另一台机器/另一个盘，
            // 只在缺件时给按钮，等于逼人去搜官网。
            ui.horizontal_wrapped(|ui| {
                ui.label("手动安装：");
                if ui
                    .button("下载 VS 生成工具（含 MSVC）")
                    .on_hover_text(format!("{VS_BUILD_TOOLS_URL}\n（已装也能用：重装/换盘/换机器）"))
                    .clicked()
                {
                    crate::proc::open_url(VS_BUILD_TOOLS_URL);
                }
                if ui
                    .button("下载 Windows SDK")
                    .on_hover_text(format!("{SDK_URL}\n（已装也能用：重装/换盘/换机器）"))
                    .clicked()
                {
                    crate::proc::open_url(SDK_URL);
                }
            });
            ui.label(
                egui::RichText::new(if ready {
                    "当前这套已经能用了；上面两个地址留着重装或换机器时用。\
                     装 VS 生成工具时勾「使用 C++ 的桌面开发」，它自带 MSVC 和 Windows SDK。"
                } else {
                    "装 VS 生成工具时勾「使用 C++ 的桌面开发」，它自带 MSVC 和 Windows SDK；\
                     装完回来点「自动探测」就能填好。浏览器没弹出来，就手动打开上面悬停显示的地址。"
                })
                .small()
                .weak(),
            );
            ui.horizontal(|ui| {
                if ui.button("选择 link.exe 目录").clicked() {
                    pick = Some(0);
                }
                if ui.button("选择 MSVC lib 目录").clicked() {
                    pick = Some(1);
                }
                if ui.button("选择 SDK lib 目录").clicked() {
                    pick = Some(2);
                }
                if ui.button("自动探测").clicked() {
                    do_guess = true;
                }
                if ui.button("清除").clicked() {
                    do_clear = true;
                }
            });
        });

        if let Some(which) = pick {
            let (title, hint) = match which {
                0 => ("选择 MSVC 的 bin 目录", "里面有 link.exe"),
                1 => ("选择 MSVC 的 lib 目录", "里面有 libcmt.lib"),
                _ => ("选择 Windows SDK 的 lib 目录", "里面有 kernel32.lib"),
            };
            // 上次选的目录还在这附近的话，从那儿开始找，省得每次从头翻。
            let start = self
                .tc
                .as_ref()
                .map(|r| match which {
                    0 => r.tc.msvc_bin.clone(),
                    1 => r.tc.msvc_lib.clone(),
                    _ => r.tc.sdk_lib.clone(),
                })
                .filter(|p| !p.trim().is_empty())
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_dir());
            let mut dlg = rfd::FileDialog::new().set_title(format!("{title}（{hint}）"));
            if let Some(d) = start {
                dlg = dlg.set_directory(d);
            }
            if let Some(dir) = dlg.pick_folder() {
                let d = dir.display().to_string();
                let res = match which {
                    0 => tc::set(&self.store, Some(&d), None, None, None),
                    1 => tc::set(&self.store, None, Some(&d), None, None),
                    _ => tc::set(&self.store, None, None, Some(&d), None),
                };
                self.status = match res {
                    Ok(m) => m,
                    Err(e) => e,
                };
                self.tc = None;
            }
        }
        if do_guess {
            self.status = match tc::guess(&self.store) {
                Ok(m) => m,
                Err(e) => e,
            };
            self.tc = None;
        }
        if do_clear {
            self.status = match tc::clear(&self.store) {
                Ok(m) => m,
                Err(e) => e,
            };
            self.tc = None;
        }
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("设置");
        // 离线模式：不用 Teambition 也能跑。放最上面——它决定下面这些还连不连网。
        let env_forced = std::env::var("TBA_OFFLINE").is_ok();
        let mut off = self.offline;
        let resp = ui
            .add_enabled(
                !env_forced,
                egui::Checkbox::new(&mut off, "离线模式（不用 Teambition）"),
            )
            .on_hover_text(
                "勾上就不连 Teambition：项目列表、项目能力、开工文案全部走本地缓存，\
                 通用能力和编译 & 重载本来就是本地的，照旧可用。\n\
                 需要云端才成立的操作（领任务、done、ask/wait）会提示不可用。\n\
                 缓存是联网时自动存的——所以第一次用某个项目前，先联网打开过一次。",
            );
        if resp.changed() {
            let mut cfg = self.store.load_config();
            cfg.offline = off;
            match cfg.save(&self.store.root) {
                Ok(()) => {
                    self.offline = off;
                    // 立刻生效：token 门禁、项目列表、能力缓存全跟着切。
                    self.needs_token = !off && cfg.token().is_none();
                    self.skills.clear();
                    self.reload_projects();
                    self.status = if off {
                        "已切到离线模式：只用本地缓存".to_string()
                    } else {
                        "已切回在线模式：重新连 Teambition".to_string()
                    };
                }
                Err(e) => self.status = format!("保存失败: {e}"),
            }
        }
        if env_forced {
            ui.label(
                egui::RichText::new(
                    "当前由环境变量 TBA_OFFLINE 强制指定，这里的勾选不生效（去掉环境变量即可）",
                )
                .small()
                .weak(),
            );
        }
        ui.label(
            egui::RichText::new(if self.offline {
                "离线中：项目与能力来自本地缓存；开工文案照常能复制。"
            } else {
                "在线：联网读写 Teambition。"
            })
            .small()
            .weak(),
        );
        ui.add_space(8.0);
        // 工具链紧跟其后：编译不了是最容易卡住人的一件事，比字号重要。
        self.draw_toolchain(ui);
        ui.add_space(12.0);
        // GitButler / git 也从主界面搬到这里：优先级仍然最高（缺它 TBHub 不干活），
        // 但它平时是「装好就再也不看」的东西，不该常年占着项目列表上方。
        self.draw_gitbutler_card(ui);
        ui.add_space(12.0);
        ui.label(
            "界面偏好会单独存到 data/ui.toml，不会碰 Teambition 配置（token 等）。",
        );
        ui.add_space(12.0);

        egui::Grid::new("settings_form")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("字号 (px)");
                ui.horizontal(|ui| {
                    // DragValue：按住左右拖就增减，点一下也能直接打字。
                    // 不用 Slider——滑块占一整行，拖到 18 这种精确值很难。
                    let mut size = self.font_size;
                    let resp = ui.add(
                        egui::DragValue::new(&mut size)
                            .speed(1.0)
                            .range(MIN_FONT_SIZE..=MAX_FONT_SIZE)
                            .fixed_decimals(0)
                            .suffix(" px"),
                    );
                    if resp.changed() {
                        let v = size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                        if (v - self.font_size).abs() > 0.01 {
                            self.font_size = v;
                            save_prefs(
                                &self.store,
                                &UiPrefs {
                                    font_size: v,
                                    font: self.font_choice.clone(),
                                },
                            );
                        }
                    }
                    if ui.button("-").on_hover_text("小一号").clicked() {
                        self.font_size = (self.font_size - 1.0).max(MIN_FONT_SIZE);
                        save_prefs(
                            &self.store,
                            &UiPrefs {
                                font_size: self.font_size,
                                font: self.font_choice.clone(),
                            },
                        );
                    }
                    if ui.button("+").on_hover_text("大一号").clicked() {
                        self.font_size = (self.font_size + 1.0).min(MAX_FONT_SIZE);
                        save_prefs(
                            &self.store,
                            &UiPrefs {
                                font_size: self.font_size,
                                font: self.font_choice.clone(),
                            },
                        );
                    }
                });
                ui.end_row();

                ui.label("中文字体");
                let cur_label = FONT_CHOICES
                    .iter()
                    .find(|c| c.0 == self.font_choice)
                    .map(|c| c.1.to_string())
                    .unwrap_or_else(|| "自动（黑体优先）".to_string());
                egui::ComboBox::from_id_salt("font_pick")
                    .selected_text(cur_label)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.font_choice.is_empty(), "自动（黑体优先）")
                            .clicked()
                        {
                            self.font_choice.clear();
                            save_prefs(
                                &self.store,
                                &UiPrefs {
                                    font_size: self.font_size,
                                    font: self.font_choice.clone(),
                                },
                            );
                        }
                        for (key, label, path) in FONT_CHOICES {
                            // 本机没装的字体也列出来，但标一下，免得选了没反应还不知道为什么。
                            let ok = std::path::Path::new(path).exists();
                            let text = if ok {
                                (*label).to_string()
                            } else {
                                format!("{label}（未安装）")
                            };
                            if ui
                                .selectable_label(self.font_choice == *key, text)
                                .clicked()
                            {
                                self.font_choice = (*key).to_string();
                                save_prefs(
                                    &self.store,
                                    &UiPrefs {
                                        font_size: self.font_size,
                                        font: self.font_choice.clone(),
                                    },
                                );
                            }
                        }
                    });
                ui.end_row();

                ui.label("预览");
                ui.label(
                    egui::RichText::new("这是一段示例中文，用来观察字号效果。")
                        .size(self.font_size),
                );
                ui.end_row();
            });

        ui.add_space(12.0);
        ui.label(format!(
            "当前字号: {} px（范围 {}–{}）",
            self.font_size as i32,
            MIN_FONT_SIZE as i32,
            MAX_FONT_SIZE as i32
        ));
        ui.add_space(8.0);
        ui.label("提示: 字号变大后，侧栏、按钮和间距都会按比例放大。");

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("邮件通知");
        ui.label("到「待回复」这一步自动发邮件给客户，客户直接回邮件也能续上任务。");
        ui.label(
            egui::RichText::new(
                "注意：SMTP 只能发信，收不到回信。要让「邮件回复」也能继续，必须再填 IMAP（不填就只发不收）。",
            )
            .small(),
        );
        ui.add_space(6.0);

        egui::Grid::new("mail_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("SMTP 主机");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.mail.smtp_host)
                            .desired_width(300.0)
                            .hint_text("smtp.qq.com"),
                    );
                    ui.label("端口");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.mail.smtp_port)
                            .desired_width(70.0)
                            .hint_text("465"),
                    );
                });
                ui.end_row();

                ui.label("账号");
                ui.add(
                    egui::TextEdit::singleline(&mut self.mail.smtp_user)
                        .desired_width(380.0)
                        .hint_text("you@example.com"),
                );
                ui.end_row();

                ui.label("密码/授权码");
                ui.add(
                    egui::TextEdit::singleline(&mut self.mail.smtp_pass)
                        .password(true)
                        .desired_width(380.0)
                        .hint_text("邮箱授权码（不是登录密码）"),
                );
                ui.end_row();

                ui.label("发件人");
                ui.add(
                    egui::TextEdit::singleline(&mut self.mail.smtp_from)
                        .desired_width(380.0)
                        .hint_text("留空 = 用账号"),
                );
                ui.end_row();

                ui.label("收件人");
                ui.add(
                    egui::TextEdit::singleline(&mut self.mail.mail_to)
                        .desired_width(380.0)
                        .hint_text("客户邮箱，多个用逗号分隔"),
                );
                ui.end_row();

                ui.label("IMAP 主机");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.mail.imap_host)
                            .desired_width(300.0)
                            .hint_text("imap.qq.com"),
                    );
                    ui.label("端口");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.mail.imap_port)
                            .desired_width(70.0)
                            .hint_text("993"),
                    );
                });
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.mail.mail_on_ask, "到「待回复」时自动发信");
            if ui.button("保存邮件配置").clicked() {
                // load-modify-save：只改邮件字段，token / 列名 / 工具映射原样保留。
                let mut cfg = self.store.load_config();
                self.mail.apply_to(&mut cfg);
                self.status = match cfg.save(&self.store.root) {
                    Ok(()) => "邮件配置已保存到 data/config.toml。".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                };
            }
            let send = ui.add_enabled(!self.mail_busy, egui::Button::new("发送测试邮件"));
            if send.clicked() {
                let mut cfg = self.store.load_config();
                self.mail.apply_to(&mut cfg);
                let _ = cfg.save(&self.store.root);
                self.mail_busy = true;
                self.status = "正在发送测试邮件...".to_string();
                let tx = self.tx.clone();
                // 网络 IO 放后台：SMTP 握手 + 投递可能要好几秒，不能卡住 UI 帧。
                thread::spawn(move || {
                    let r = crate::mail::send(
                        &cfg,
                        "[TB] 测试邮件",
                        "这是一封 tba 测试邮件，收到即表示 SMTP 配置可用。",
                    );
                    let _ = tx.send(Ev::MailSent(r));
                });
            }
            if self.mail_busy {
                ui.spinner();
            }
        });
        ui.add_space(4.0);
        // 用表单里的值判断（不读磁盘）：这样改了还没保存也能立刻看到「能不能发/收」。
        let mut probe = crate::config::Config::default();
        self.mail.apply_to(&mut probe);
        ui.label(format!(
            "能发信: {} / 能收信: {}（收信靠 IMAP，密码跟 SMTP 共用）",
            if crate::mail::can_send(&probe) { "是" } else { "否" },
            if crate::mail::can_receive(&probe) {
                "是"
            } else {
                "否"
            }
        ));
        ui.label("命令行也能验：tba mail status / tba mail test / tba mail check");

        // ---- 等待与轮询 ----
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.heading("等待与轮询");
        ui.label("「待回复」这一步 agent 会一直等。这里的参数决定等多久、多久查一次。");
        ui.add_space(6.0);

        egui::Grid::new("wait_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("--once 单轮最长等待");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.wait.timeout_secs)
                            .desired_width(70.0)
                            .hint_text("540"),
                    );
                    ui.label("秒　仅 --once 生效；默认常驻监听不超时");
                });
                ui.end_row();

                ui.label("评论轮询间隔");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.wait.interval_secs)
                            .desired_width(70.0)
                            .hint_text("5"),
                    );
                    ui.label("秒　多久查一次你在 Teambition 的回复");
                });
                ui.end_row();

                ui.label("邮件轮询间隔");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.wait.mail_poll_secs)
                            .desired_width(70.0)
                            .hint_text("5"),
                    );
                    ui.label("秒　只抓提问水位之后的新信，下限 5 秒");
                });
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("保存等待参数").clicked() {
                let mut cfg = self.store.load_config();
                self.wait.apply_to(&mut cfg);
                self.status = match cfg.save(&self.store.root) {
                    Ok(()) => "等待参数已保存。下次 tba ask / tba wait 生效。".to_string(),
                    Err(e) => format!("保存失败: {e}"),
                };
            }
            if ui.button("恢复默认").clicked() {
                self.wait = WaitForm::from_cfg(&crate::config::Config::default());
                self.status = "已填回默认值，点「保存等待参数」才写入。".to_string();
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "默认 {} / {} / {} 秒。命令行可临时覆盖：tba wait <claim> --timeout 300 --interval 10",
                crate::config::DEFAULT_WAIT_TIMEOUT_SECS,
                crate::config::DEFAULT_WAIT_INTERVAL_SECS,
                crate::config::DEFAULT_MAIL_POLL_SECS
            ))
            .small(),
        );
    }
}

// 双击 exe 时 Windows 会先给一个控制台黑窗，再弹 GUI，很难看。
// exe 是 console 子系统（CLI 要 stdout），没法靠 windows_subsystem 属性关掉——
// 那会把所有子命令的输出也吞了。只能在进 GUI 之前主动把窗口藏掉 + FreeConsole。
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn FreeConsole() -> i32;
    fn GetConsoleWindow() -> isize;
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
}

#[cfg(windows)]
pub fn hide_console() {
    // 顺序有讲究：先 ShowWindow(SW_HIDE) 再 FreeConsole。
    // FreeConsole 只是把本进程从控制台摘下来，窗口要等「最后一个附着的进程」退出才销毁；
    // 先藏一下能保证眼睛立刻看不到，不依赖 FreeConsole 的时序。
    const SW_HIDE: i32 = 0;
    unsafe {
        let h = GetConsoleWindow();
        if h != 0 {
            ShowWindow(h, SW_HIDE);
        }
        FreeConsole();
    }
}

#[cfg(not(windows))]
pub fn hide_console() {}

pub fn run(store: &Store) -> Result<(), String> {
    hide_console();
    let store = store.clone();
    // 截图自验用：窗口大小可指定（TBA_UI_SIZE=1000x1500），否则默认 920x640。
    // 卡片展开后有 tb skill / 通用 skill / 编译 & 重载 三块，默认高度截不全。
    let size = std::env::var("TBA_UI_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some([w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?])
        })
        .unwrap_or([920.0, 640.0]);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_title("tba · 项目与能力"),
        ..Default::default()
    };
    eframe::run_native(
        "tba",
        options,
        Box::new(move |cc| {
            // 先读偏好再装字体：用户选过黑体就别硬上雅黑。
            let prefs = load_prefs(&store);
            install_cjk_font(&cc.egui_ctx, &prefs.font);
            Ok(Box::new(App::new(store)))
        }),
    )
    .map_err(|e| format!("窗口启动失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 造一个没 token 的临时 App：不联网、不起后台线程，纯逻辑测。
    fn tmp_app(tag: &str) -> App {
        let root = std::env::temp_dir().join(format!("tba-ui-test-{tag}"));
        // 先清掉上次的残留：否则 ui.toml 里的旧字号会被读回来，断言就飘了。
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        App::new(Store { root })
    }

    // 等待参数：空/非法输入回落默认，别把配置写成 0 把轮询打爆。
    #[test]
    fn wait_form_falls_back_on_bad_input() {
        let mut cfg = crate::config::Config::default();
        cfg.wait_timeout_secs = 999;
        let f = WaitForm {
            timeout_secs: "".into(),
            interval_secs: "abc".into(),
            mail_poll_secs: " 25 ".into(),
        };
        f.apply_to(&mut cfg);
        assert_eq!(
            cfg.wait_timeout_secs,
            crate::config::DEFAULT_WAIT_TIMEOUT_SECS,
            "空值该回落默认"
        );
        assert_eq!(
            cfg.wait_interval_secs,
            crate::config::DEFAULT_WAIT_INTERVAL_SECS,
            "非法值该回落默认"
        );
        assert_eq!(cfg.mail_poll_secs, 25, "合法值该生效（含首尾空格）");
    }

    // 合法但过小的数字也要夹到下限：填 0 会把轮询变成忙等。
    #[test]
    fn wait_form_clamps_tiny_values() {
        let mut cfg = crate::config::Config::default();
        let f = WaitForm {
            timeout_secs: "0".into(),
            interval_secs: "1".into(),
            mail_poll_secs: "0".into(),
        };
        f.apply_to(&mut cfg);
        assert_eq!(cfg.wait_timeout_secs, crate::config::MIN_WAIT_TIMEOUT_SECS);
        assert_eq!(cfg.wait_interval_secs, crate::config::MIN_WAIT_INTERVAL_SECS);
        assert_eq!(cfg.mail_poll_secs, crate::config::MIN_MAIL_POLL_SECS);
    }

    // 表单和配置要能来回转，不能改一次就漂。
    #[test]
    fn wait_form_roundtrips_through_config() {
        let mut cfg = crate::config::Config::default();
        cfg.wait_timeout_secs = 300;
        cfg.wait_interval_secs = 8;
        cfg.mail_poll_secs = 30;
        let f = WaitForm::from_cfg(&cfg);
        assert_eq!(f.timeout_secs, "300");

        let mut back = crate::config::Config::default();
        f.apply_to(&mut back);
        assert_eq!(back.wait_timeout_secs, 300);
        assert_eq!(back.wait_interval_secs, 8);
        assert_eq!(back.mail_poll_secs, 30);
    }

    // 「复制文案(本次任务·中文)」：界面里有草稿 -> 文案带草稿 + 走 start。
    #[test]
    fn copy_prompt_this_task_includes_draft() {
        let mut app = tmp_app("this");
        let pid = "p-1";
        app.task_drafts
            .insert(pid.to_string(), "# 界面里的任务标题\n正文第二行".to_string());
        let (text, has_task) = app.copy_prompt(pid, "项目一", CopyMode::ThisTask, Lang::Zh);
        assert!(has_task, "填了草稿就该带上本次任务");
        assert!(text.contains("本次任务"), "文案应有本次任务段");
        assert!(text.contains("start"), "应让 agent 跑 start");
        assert!(text.contains("界面里的任务标题"), "标题应进文案");
    }

    // 「复制文案(领列1·中文)」：即使界面里还留着草稿，也必须丢掉。
    // 否则 agent 会新建任务，而不是去队列领，行为跑偏。
    #[test]
    fn copy_prompt_queue_ignores_leftover_draft() {
        let mut app = tmp_app("queue");
        let pid = "p-2";
        app.task_drafts
            .insert(pid.to_string(), "# 残留草稿\n不该出现".to_string());
        let (text, has_task) = app.copy_prompt(pid, "项目二", CopyMode::Queue, Lang::Zh);
        assert!(!has_task, "领列1任务 不该带草稿");
        assert!(text.contains("claim"), "应让 agent 跑 claim");
        assert!(!text.contains("残留草稿"), "残留草稿不该进文案");
        assert!(!text.contains("不该出现"), "残留正文不该进文案");
    }

    // 两个模式按钮的产出必须真的不同。
    #[test]
    fn two_buttons_produce_different_text() {
        let mut app = tmp_app("diff");
        let pid = "p-3";
        app.task_drafts
            .insert(pid.to_string(), "# 差异任务\n正文".to_string());
        let (a, _) = app.copy_prompt(pid, "项目三", CopyMode::ThisTask, Lang::Zh);
        let (b, _) = app.copy_prompt(pid, "项目三", CopyMode::Queue, Lang::Zh);
        assert_ne!(a, b, "两个按钮不该产出同一份文案");
    }

    // 中英两版：规则都在（命令 / 阶段名 / 禁自建任务），但英文必须更短（省 token）。
    #[test]
    fn language_variants_differ_but_keep_rules() {
        let mut app = tmp_app("lang");
        let pid = "p-5";
        let (zh, _) = app.copy_prompt(pid, "项目五", CopyMode::Queue, Lang::Zh);
        let (en, _) = app.copy_prompt(pid, "项目五", CopyMode::Queue, Lang::En);
        assert_ne!(zh, en, "中英两版不该相同");
        for t in [&zh, &en] {
            for cmd in ["claim --project", "take <", "beat <claim_id>", "ask <claim_id>"] {
                assert!(t.contains(cmd), "两版都必须有命令: {cmd}");
            }
            // 阶段名两版都保持中文（Teambition 上的真实名字）
            assert!(t.contains("未完成"), "两版都要写清领的是「未完成」");
            assert!(t.contains("待回复"), "两版都要有「待回复」");
            assert!(t.contains("已解决") && t.contains("已完成"), "两版都要有收尾状态名");
        }
        // 长度差异由 ops 的 est_tokens 断言守着（中文一字符信息量大，比字符数没意义）。
    }

    // 空草稿时，「本次任务」按钮退化成「去队列领」，不报错。
    #[test]
    fn copy_prompt_empty_draft_falls_back_to_queue() {
        let mut app = tmp_app("empty");
        let (text, has_task) = app.copy_prompt("p-4", "项目四", CopyMode::ThisTask, Lang::Zh);
        assert!(!has_task);
        assert!(text.contains("claim"));
    }

    // 字号偏好：默认 18，落盘到 ui.toml，读回来还是 18；越界要被夹住。
    #[test]
    fn ui_prefs_roundtrip_keeps_font_size() {
        let app = tmp_app("prefs");
        assert_eq!(app.font_size, DEFAULT_FONT_SIZE, "默认字号应为 18");
        save_prefs(
            &app.store,
            &UiPrefs {
                font_size: DEFAULT_FONT_SIZE,
                ..UiPrefs::default()
            },
        );
        let back = load_prefs(&app.store);
        assert_eq!(back.font_size, DEFAULT_FONT_SIZE);

        // 写一个越界值，重新构造 App 时应被夹回区间内。
        save_prefs(
            &app.store,
            &UiPrefs {
                font_size: 999.0,
                ..UiPrefs::default()
            },
        );
        let app2 = App::new(app.store.clone());
        assert_eq!(app2.font_size, MAX_FONT_SIZE, "超上限应夹到 40");
        save_prefs(
            &app.store,
            &UiPrefs {
                font_size: 1.0,
                ..UiPrefs::default()
            },
        );
        let app3 = App::new(app.store.clone());
        assert_eq!(app3.font_size, MIN_FONT_SIZE, "低于下限应夹到 12");
    }

    // 字体选择要落盘、读回来、且默认是「自动」（空串）。
    // 存 key 不存路径：换机器路径会变，key 还能回落。
    #[test]
    fn ui_prefs_keeps_font_choice() {
        let app = tmp_app("fontpref");
        assert!(app.font_choice.is_empty(), "默认应为自动");
        save_prefs(
            &app.store,
            &UiPrefs {
                font_size: 18.0,
                font: "simhei".to_string(),
            },
        );
        let back = load_prefs(&app.store);
        assert_eq!(back.font, "simhei");
        let app2 = App::new(app.store.clone());
        assert_eq!(app2.font_choice, "simhei");
        // 老 ui.toml 只有 font_size 时不能因为缺 font 字段解析失败。
        std::fs::write(prefs_path(&app.store), "font_size = 20.0\n").unwrap();
        let old = load_prefs(&app.store);
        assert_eq!(old.font_size, 20.0);
        assert!(old.font.is_empty(), "缺字段应回落空串");
    }

    // 能力编辑窗的保存路径：改内置条目的正文要落盘、能读回，
    // 且不能顺手把开关或内置标记改掉（那是用户自己的选择）。
    #[test]
    fn cap_edit_keeps_switch_and_builtin_flag() {
        let app = tmp_app("capedit");
        let mut c = caps::get(&app.store, "shot").expect("首次打开应灌入内置能力");
        assert!(c.on && c.builtin);
        c.on = false;
        c.body = "# 我改过的截图
只写这一行".to_string();
        caps::upsert(&app.store, c).unwrap();

        let back = caps::get(&app.store, "shot").unwrap();
        assert_eq!(back.body, "# 我改过的截图
只写这一行");
        assert!(!back.on, "编辑正文不该把用户关掉的开关打开");
        assert!(back.builtin, "内置标记要留着，否则没法恢复");
    }

    // 关掉的能力不能出现在 agent 文案里（省 token 也防误用）；
    // 开着的那几条只出名字，不出正文。
    #[test]
    fn disabled_cap_is_not_in_prompt() {
        let app = tmp_app("capprompt");
        caps::set_on(&app.store, "shot", false).unwrap();
        let text = crate::ops::agent_prompt_from(&app.store, "p1", "proj", &[], None, Lang::Zh);
        // 能力现在是「标题 + 一行一条」，所以按段落取，不按单行取。
        let seg = text
            .split("

")
            .find(|b| b.contains("通用能力"))
            .expect("文案里应该有通用能力这一段");
        assert!(!seg.contains("shot"), "关掉的能力不该出现在名单里: {seg}");
        assert!(seg.contains("git-main"), "开着的能力应该在名单里: {seg}");
        assert!(
            !text.contains("## 一条命令"),
            "能力正文不该内联进文案，否则 token 白烧"
        );
    }

    // 编译 & 重载：设置里有编译指令就写进文案（agent 不用自己猜 rust/zig/go）；
    // 停用后整段消失。
    #[test]
    fn setting_line_in_prompt() {
        let app = tmp_app("settingprompt");
        let mut st = crate::setting::ProjectSetting::default();
        st.compile = "zig build -Doptimize=ReleaseFast".to_string();
        st.root = r"D:\dev\z".to_string();
        st.name = "z".to_string();
        st.cache(&app.store, "p1").unwrap();

        let text = crate::ops::agent_prompt_from(&app.store, "p1", "proj", &[], None, Lang::Zh);
        assert!(text.contains("编译 & 重载"), "应有一段编译说明: {text}");
        assert!(text.contains("zig build"), "应带编译指令: {text}");

        st.on = false;
        st.cache(&app.store, "p1").unwrap();
        let off = crate::ops::agent_prompt_from(&app.store, "p1", "proj", &[], None, Lang::Zh);
        assert!(!off.contains("编译 & 重载"), "停用后不该出现: {off}");
    }

    // 项目级取消勾选 = 这个项目不用：文案里不能出现，否则 agent 还是会去读。
    #[test]
    fn project_cap_off_hides_it_from_prompt() {
        let app = tmp_app("capprompt2");
        app.store.set_cap_off("p1", "shot", true).unwrap();
        let text = crate::ops::agent_prompt_from(&app.store, "p1", "proj", &[], None, Lang::Zh);
        let seg = text
            .split("

")
            .find(|b| b.contains("通用能力"))
            .expect("文案里应该有通用能力这一段");
        assert!(!seg.contains("shot"), "本项目关掉的能力不该出现: {seg}");
        assert!(seg.contains("git-main"), "其他能力照旧: {seg}");

        // 另一个项目不受影响（项目级，不是全局）
        let other = crate::ops::agent_prompt_from(&app.store, "p2", "proj2", &[], None, Lang::Zh);
        let seg2 = other
            .split("

")
            .find(|b| b.contains("通用能力"))
            .expect("文案里应该有通用能力这一段");
        assert!(seg2.contains("shot"), "别的项目应该还在: {seg2}");
    }
}
