use crate::ops::{agent_prompt_from, draft_for_mode, list_skills, CopyMode, SkillItem};
use crate::oauth::TOKEN_PAGE;
use crate::store::Store;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

// 默认字号。egui 自带约 14pt，中文在这个尺寸下很吃力，所以默认给 22。
const DEFAULT_FONT_SIZE: f32 = 22.0;
const MIN_FONT_SIZE: f32 = 12.0;
const MAX_FONT_SIZE: f32 = 40.0;
// 按默认字号算的布局基准，用来等比放大控件间距。
const BASE_FONT_SIZE: f32 = 14.0;

enum Ev {
    Projects(Vec<(String, String)>),
    Skills(String, Vec<SkillItem>),
    LoggedIn(usize),
    Err(String),
    ReloadDone(String),
    /// GitButler/git 后台检测结果
    GbChecked {
        gb: crate::gitbutler::GbStatus,
        git: Option<String>,
    },
    /// GitButler CLI 后台安装结果
    GbInstalled(Result<(), String>),
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Projects,
    Reload,
    Settings,
}

// 界面偏好。单独存 data/ui.toml，不碰 config.toml（那里有 token 和工具映射，
// 整体覆写风险太大）。
#[derive(Clone, Serialize, Deserialize)]
struct UiPrefs {
    #[serde(default = "default_font_size")]
    font_size: f32,
}

fn default_font_size() -> f32 {
    DEFAULT_FONT_SIZE
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            font_size: DEFAULT_FONT_SIZE,
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
    token_input: String,
    setup_busy: bool,
    projects: Vec<(String, String)>,
    skills: HashMap<String, Vec<SkillItem>>,
    // 每个项目卡片的「本次任务」草稿。留空 = 让 agent 去队列（列「待处理」）领。
    task_drafts: HashMap<String, String>,
    open: HashMap<String, bool>,
    loading_projects: bool,
    loading_skill: Option<String>,
    status: String,
    // 侧栏视图 + 通用能力（重载）
    view: View,
    reload_root: String,
    reload_name: String,
    reload_live: String,
    reload_mode: String,
    reload_args: String,
    reload_out: String,
    reload_log: String,
    reload_busy: bool,
    reload_elevate: bool,
    // 界面偏好（字号）
    font_size: f32,
    // 已经应用到 egui 的字号，避免每帧重复 set_style
    applied_font: f32,
    // GitButler 绑定（优先级高于 Teambition）：None = 还没检测完
    gb: Option<crate::gitbutler::GbStatus>,
    git_ver: Option<String>,
    gb_busy: bool,
    gb_msg: String,
}

// 按候选顺序找第一个存在的字体装上。中文放字体链最前，
// 英文和数字仍可回退到 egui 内置字体。
fn install_cjk_font(ctx: &egui::Context) {
    let candidates: &[&str] = &[
        "C:/Windows/Fonts/Deng.ttf",
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simsun.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    ];
    let mut fonts = egui::FontDefinitions::default();
    for path in candidates {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.len() < 1024 {
            continue;
        }
        fonts.font_data.insert(
            "cjk".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let chain = fonts.families.entry(fam).or_default();
            if !chain.iter().any(|f| f == "cjk") {
                chain.insert(0, "cjk".to_owned());
            }
        }
        eprintln!("[tb ui] 已加载中文字体: {path}");
        break;
    }
    ctx.set_fonts(fonts);
}

impl App {
    pub fn new(store: Store) -> App {
        let (tx, rx) = channel();
        // 先用缓存秒开，再后台刷新。
        let cached = store.load_projects_cache();
        let needs_token = store.load_config().token().is_none();
        // 重载面板默认指向当前 exe 所在项目
        let reload_root = crate::capability::guess_root();
        let reload_name = crate::capability::guess_name(&reload_root);
        let reload_live = crate::capability::default_live(&reload_root, &reload_name);
        let reload_args = if reload_name == "tb" {
            "ui".to_string()
        } else {
            String::new()
        };
        let last = crate::capability::last_status();
        let prefs = load_prefs(&store);
        let font_size = prefs.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        let mut app = App {
            store,
            tx,
            rx,
            needs_token,
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
            } else {
                "正在连接 Teambition...".to_string()
            },
            view: View::Projects,
            reload_root,
            reload_name,
            reload_live,
            reload_mode: "rename".to_string(),
            reload_args,
            reload_out: if last.is_empty() {
                "尚未执行。".to_string()
            } else {
                last
            },
            reload_log: String::new(),
            reload_busy: false,
            reload_elevate: false,
            font_size,
            applied_font: 0.0,
            gb: None,
            git_ver: None,
            gb_busy: false,
            gb_msg: String::new(),
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

    /// GitButler 状态卡：CLI 装没装、什么版本、从哪来、当前目录配没配；
    /// 缺的时候给「安装到运行目录」一键装。git 是它的系统级依赖，一起显示。
    fn draw_gitbutler_card(&mut self, ui: &mut egui::Ui) {
        // 先克隆出来，避免闭包里同时借 self 和改 self
        let gb = self.gb.clone();
        let mut want_check = false;
        let mut want_install = false;
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
                        ui.colored_label(
                            egui::Color32::from_rgb(0x2e, 0x7d, 0x32),
                            format!("✓ but {ver}"),
                        );
                        ui.label(format!("来源: {}", g.source.map(|s| s.label()).unwrap_or("?")));
                        match g.project_setup {
                            Some(true) => {
                                ui.label("· 工作区已配置");
                            }
                            Some(false) => {
                                ui.label("· 当前目录未配置（but setup）");
                            }
                            None => {}
                        }
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
            // 缺 CLI 时给一键安装
            let missing = gb.as_ref().map(|g| !g.found).unwrap_or(false);
            if missing && !self.gb_busy {
                ui.horizontal(|ui| {
                    if ui.button("安装到运行目录 (data/bin)").clicked() {
                        want_install = true;
                    }
                    if ui.small_button("复制官方安装命令").clicked() {
                        ui.ctx()
                            .copy_text(crate::gitbutler::OFFICIAL_INSTALL_CMD.to_string());
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Windows 官方只发 MSI：点安装会自动下载并抽取 but.exe 到 data/bin，\
                         无需管理员、不动注册表。官方一键命令仅 mac/linux 可用。",
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
    }

    fn reload_projects(&mut self) {
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
                Ev::ReloadDone(text) => {
                    self.reload_busy = false;
                    self.reload_out = text;
                    self.reload_log = crate::capability::log_tail(&self.reload_root, 15);
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
            }
        }
        // 后台在工作时保持界面响应
        if self.loading_projects || self.loading_skill.is_some() || self.setup_busy || self.reload_busy
        {
            ctx.request_repaint();
        }
        if self.gb_busy || self.gb.is_none() {
            ctx.request_repaint();
        }

        // 字号变了才重设样式。egui 默认约 14pt，中文在这个尺寸下很吃力。
        if (self.applied_font - self.font_size).abs() > 0.01 {
            let s = self.font_size;
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
                ui.heading("TBHub");
                ui.add_space(10.0);

                ui.label(egui::RichText::new("项目").small().weak());
                if ui
                    .selectable_label(self.view == View::Projects, "项目列表")
                    .clicked()
                {
                    self.view = View::Projects;
                }

                ui.add_space(12.0);
                ui.label(egui::RichText::new("通用能力").small().weak());
                if ui
                    .selectable_label(self.view == View::Reload, "重载")
                    .on_hover_text("编译 -> 杀旧 -> 复制校验 -> 启动")
                    .clicked()
                {
                    self.view = View::Reload;
                }

                ui.add_space(12.0);
                ui.label(egui::RichText::new("设置").small().weak());
                if ui
                    .selectable_label(self.view == View::Settings, "⚙ 设置")
                    .on_hover_text("字体大小等界面偏好")
                    .clicked()
                {
                    self.view = View::Settings;
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(if self.needs_token {
                            "未登录"
                        } else {
                            "已登录"
                        })
                        .small()
                        .weak(),
                    );
                });
            });

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("TBHub · Teambition 共享 Skill");
                if !self.needs_token {
                    if ui.button("刷新").clicked() {
                        self.reload_projects();
                    }
                    if ui.button("重新登录").clicked() {
                        self.logout();
                    }
                }
            });
            ui.label(&self.status);
            ui.separator();
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // GitButler 状态卡：优先级高于 Teambition，哪个视图都置顶显示，
            // 缺 CLI 时给一键安装（装到运行目录，无需管理员）。
            self.draw_gitbutler_card(ui);
            ui.add_space(6.0);
            if self.view == View::Settings {
                self.draw_settings(ui);
                return;
            }
            if self.view == View::Reload {
                self.draw_reload(ui);
                return;
            }
            if self.needs_token {
                self.draw_setup(ui);
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
                let projects = self.projects.clone();
                let mut to_load: Option<String> = None;
                // Option 三元组: 项目id, 项目名, 复制模式。
                // CopyMode::Queue => 复制「领列1任务」文案（忽略本次任务草稿）；
                // CopyMode::ThisTask => 复制「本次任务」文案（有草稿就带草稿）。
                let mut to_copy: Option<(String, String, CopyMode)> = None;
                for (pid, pname) in projects {
                    let pid = pid.clone();
                    let pname = pname.clone();
                    let open = self.open.get(&pid).cloned().unwrap_or(false);
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
                        ui.indent(pid.clone(), |ui| {
                            ui.label(format!(
                                "共享记忆目录: {}",
                                self.store.memory_dir(&pid).display()
                            ));
                            ui.add_space(4.0);
                            ui.label(if loading {
                                "正在加载 skill...".to_string()
                            } else if rows.is_empty() {
                                "没有 skill。".to_string()
                            } else {
                                format!("skill ({} 个):", rows.len())
                            });
                            for row in &rows {
                                let mut on = row.on;
                                if ui.checkbox(&mut on, &row.name).changed() {
                                    let _ = self.store.set_switch(&pid, &row.name, on);
                                    if let Some(list) = self.skills.get_mut(&pid) {
                                        if let Some(r) =
                                            list.iter_mut().find(|r| r.name == row.name)
                                        {
                                            r.on = on;
                                        }
                                    }
                                    self.status = format!(
                                        "skill「{}」已{}",
                                        row.name,
                                        if on { "开启" } else { "关闭" }
                                    );
                                }
                            }
                            ui.add_space(6.0);
                            ui.label("本次任务（留空 = 让 agent 去队列「待处理」领）:");
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
                            ui.horizontal(|ui| {
                                if ui.button("复制agent文案(本次任务)").clicked() {
                                    to_copy = Some((pid.clone(), pname.clone(), CopyMode::ThisTask));
                                }
                                if ui.button("复制agent文案(领列1任务)").clicked() {
                                    to_copy = Some((pid.clone(), pname.clone(), CopyMode::Queue));
                                }
                            });
                        });
                    }
                    ui.separator();
                }
                if let Some(pid) = to_load {
                    self.load_skills(&pid);
                }
                if let Some((pid, pname, mode)) = to_copy {
                    let (text, has_task) = self.copy_prompt(&pid, &pname, mode);
                    ui.ctx().copy_text(text);
                    self.status = if has_task {
                        "已复制 Agent 文案（含本次任务，agent 会新建任务开工）。".to_string()
                    } else {
                        "已复制 Agent 文案（agent 会去队列「待处理」领任务）。".to_string()
                    };
                }
            });
        });
    }
}

impl App {
    // 两个「复制 agent 文案」按钮共用的动作。
    //
    // 抽成方法是为了能直接测：按钮本身只负责把返回值塞进剪贴板，
    // 「本次任务 vs 领列1任务」的行为差异全在这里，一处说了算。
    //
    // 返回 (文案, 是否带上了「本次任务」)，第二个值用来决定底部状态提示。
    fn copy_prompt(&mut self, pid: &str, pname: &str, mode: CopyMode) -> (String, bool) {
        let rows = self.skills.get(pid).cloned().unwrap_or_default();
        // 草稿的组装交给 draft_for_mode：Queue 模式强制丢弃界面里的草稿，
        // ThisTask 模式按草稿是否填写决定带不带。UI 不再重复这段逻辑。
        let raw = self.task_drafts.get(pid).cloned().unwrap_or_default();
        let draft = draft_for_mode(&self.store, &raw, mode);
        let has_task = draft.is_some();
        let text = agent_prompt_from(&self.store, pid, pname, &rows, draft.as_ref());
        (text, has_task)
    }

    // 通用能力：重载。编译 -> 杀旧 -> 复制校验 -> 启动。
    fn draw_reload(&mut self, ui: &mut egui::Ui) {
        ui.heading("重载");
        ui.label("编译 → 杀旧 → 复制校验 → 启动。退出码是唯一真值，cargo 输出只进 target/reload.log。");
        ui.add_space(10.0);

        egui::Grid::new("reload_form")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label("项目目录");
                ui.add(egui::TextEdit::singleline(&mut self.reload_root).desired_width(430.0));
                ui.end_row();

                ui.label("exe 基名");
                ui.add(egui::TextEdit::singleline(&mut self.reload_name).desired_width(200.0));
                ui.end_row();

                ui.label("模式");
                egui::ComboBox::from_id_salt("reload_mode")
                    .selected_text(if self.reload_mode == "rename" {
                        "rename · 改名直跑"
                    } else {
                        "copy · 副本运行"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.reload_mode,
                            "rename".to_string(),
                            "rename · 改名直跑",
                        );
                        ui.selectable_value(
                            &mut self.reload_mode,
                            "copy".to_string(),
                            "copy · 副本运行",
                        );
                    });
                ui.end_row();

                if self.reload_mode != "rename" {
                    ui.label("运行副本");
                    ui.add(egui::TextEdit::singleline(&mut self.reload_live).desired_width(430.0));
                    ui.end_row();
                }

                ui.label("启动参数");
                ui.add(egui::TextEdit::singleline(&mut self.reload_args).desired_width(200.0));
                ui.end_row();
            });

        ui.horizontal(|ui| {
            if self.reload_mode != "rename" && ui.button("重新推算副本路径").clicked() {
                self.reload_live =
                    crate::capability::default_live(&self.reload_root, &self.reload_name);
            }
            ui.checkbox(&mut self.reload_elevate, "UAC 提权");
        });
        ui.add_space(10.0);

        let busy = self.reload_busy;
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("重载 build")).clicked() {
                self.start_reload("build");
            }
            if ui.add_enabled(!busy, egui::Button::new("重启 restart")).clicked() {
                self.start_reload("restart");
            }
            if ui.add_enabled(!busy, egui::Button::new("杀掉 kill")).clicked() {
                self.start_reload("kill");
            }
            if ui.add_enabled(!busy, egui::Button::new("状态 status")).clicked() {
                self.start_reload("status");
            }
            if busy {
                ui.spinner();
            }
        });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("结果").strong());
        ui.label(egui::RichText::new(&self.reload_out).monospace());

        ui.add_space(8.0);
        ui.collapsing("reload.log 末尾 15 行", |ui| {
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(
                        egui::RichText::new(&self.reload_log).monospace().small(),
                    ));
                });
        });
    }

    fn start_reload(&mut self, action: &str) {
        self.reload_busy = true;
        self.reload_out = format!("正在执行 {action}...");
        let action = action.to_string();
        let root = self.reload_root.clone();
        let name = self.reload_name.clone();
        let live = self.reload_live.clone();
        let mode = self.reload_mode.clone();
        let args = self.reload_args.clone();
        let elevate = self.reload_elevate;
        let tx = self.tx.clone();
        thread::spawn(move || {
            // 自己重载自己：先把 cargo 产物重命名挪开，链接器才写得进去
            let release_src = action == "build" && crate::capability::is_src(&root, &name);
            match crate::capability::run_sync(
                &action,
                &root,
                &name,
                &live,
                &args,
                &mode,
                elevate,
                release_src,
            ) {
                Ok(r) => {
                    let text = if r.code == 0 {
                        r.out
                    } else if !r.err.is_empty() {
                        r.err
                    } else {
                        format!("exit {}", r.code)
                    };
                    let ok = r.code == 0;
                    let _ = tx.send(Ev::ReloadDone(text));
                    // 重载/重启成功后新实例已经起来，旧窗口让位
                    if ok && (action == "build" || action == "restart") {
                        thread::sleep(std::time::Duration::from_millis(1200));
                        std::process::exit(0);
                    }
                }
                Err(e) => {
                    let _ = tx.send(Ev::ReloadDone(format!("ERR run:{e}")));
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
            ui.label("TBHub 通过 Teambition MCP 读写项目、任务和 skill。");
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
    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("设置");
        ui.label(
            "界面偏好会单独存到 data/ui.toml，不会碰 Teambition 配置（token 等）。",
        );
        ui.add_space(12.0);

        egui::Grid::new("settings_form")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("字号 (px)");
                let resp = ui.add(
                    egui::Slider::new(&mut self.font_size, MIN_FONT_SIZE..=MAX_FONT_SIZE)
                        .step_by(1.0)
                        .text(""),
                );
                // 改动即时落盘。字号应用由 update() 的 applied_font 差异逻辑负责。
                if resp.changed() {
                    save_prefs(&self.store, &UiPrefs { font_size: self.font_size });
                }
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
    }
}

pub fn run(store: &Store) -> Result<(), String> {
    let store = store.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([920.0, 640.0])
            .with_title("TBHub - Teambition 共享 Skill"),
        ..Default::default()
    };
    eframe::run_native(
        "TBHub",
        options,
        Box::new(move |cc| {
            install_cjk_font(&cc.egui_ctx);
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
        let root = std::env::temp_dir().join(format!("tbhub-ui-test-{tag}"));
        // 先清掉上次的残留：否则 ui.toml 里的旧字号会被读回来，断言就飘了。
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        App::new(Store { root })
    }

    // 「复制agent文案(本次任务)」：界面里有草稿 -> 文案带草稿 + 走 start。
    #[test]
    fn copy_prompt_this_task_includes_draft() {
        let mut app = tmp_app("this");
        let pid = "p-1";
        app.task_drafts
            .insert(pid.to_string(), "# 界面里的任务标题\n正文第二行".to_string());
        let (text, has_task) = app.copy_prompt(pid, "项目一", CopyMode::ThisTask);
        assert!(has_task, "填了草稿就该带上本次任务");
        assert!(text.contains("This task"), "文案应有 This task 段");
        assert!(text.contains("start"), "应让 agent 跑 start");
        assert!(text.contains("界面里的任务标题"), "标题应进文案");
    }

    // 「复制agent文案(领列1任务)」：即使界面里还留着草稿，也必须丢掉。
    // 否则 agent 会新建任务，而不是去队列领，行为跑偏。
    #[test]
    fn copy_prompt_queue_ignores_leftover_draft() {
        let mut app = tmp_app("queue");
        let pid = "p-2";
        app.task_drafts
            .insert(pid.to_string(), "# 残留草稿\n不该出现".to_string());
        let (text, has_task) = app.copy_prompt(pid, "项目二", CopyMode::Queue);
        assert!(!has_task, "领列1任务 不该带草稿");
        assert!(text.contains("claim"), "应让 agent 跑 claim");
        assert!(!text.contains("残留草稿"), "残留草稿不该进文案");
        assert!(!text.contains("不该出现"), "残留正文不该进文案");
    }

    // 两个按钮的产出必须真的不同。
    #[test]
    fn two_buttons_produce_different_text() {
        let mut app = tmp_app("diff");
        let pid = "p-3";
        app.task_drafts
            .insert(pid.to_string(), "# 差异任务\n正文".to_string());
        let (a, _) = app.copy_prompt(pid, "项目三", CopyMode::ThisTask);
        let (b, _) = app.copy_prompt(pid, "项目三", CopyMode::Queue);
        assert_ne!(a, b, "两个按钮不该产出同一份文案");
    }

    // 空草稿时，「本次任务」按钮退化成「去队列领」，不报错。
    #[test]
    fn copy_prompt_empty_draft_falls_back_to_queue() {
        let mut app = tmp_app("empty");
        let (text, has_task) = app.copy_prompt("p-4", "项目四", CopyMode::ThisTask);
        assert!(!has_task);
        assert!(text.contains("claim"));
    }

    // 字号偏好：默认 22，落盘到 ui.toml，读回来还是 22；越界要被夹住。
    #[test]
    fn ui_prefs_roundtrip_keeps_font_size() {
        let app = tmp_app("prefs");
        assert_eq!(app.font_size, DEFAULT_FONT_SIZE, "默认字号应为 22");
        save_prefs(
            &app.store,
            &UiPrefs {
                font_size: DEFAULT_FONT_SIZE,
            },
        );
        let back = load_prefs(&app.store);
        assert_eq!(back.font_size, DEFAULT_FONT_SIZE);

        // 写一个越界值，重新构造 App 时应被夹回区间内。
        save_prefs(
            &app.store,
            &UiPrefs {
                font_size: 999.0,
            },
        );
        let app2 = App::new(app.store.clone());
        assert_eq!(app2.font_size, MAX_FONT_SIZE, "超上限应夹到 40");
        save_prefs(
            &app.store,
            &UiPrefs {
                font_size: 1.0,
            },
        );
        let app3 = App::new(app.store.clone());
        assert_eq!(app3.font_size, MIN_FONT_SIZE, "低于下限应夹到 12");
    }
}
