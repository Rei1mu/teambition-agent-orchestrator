// 项目设置（ProjectSetting）：编译 & 重载要用的参数，存 Teambition。
//
// 为什么放云端而不是本地配置：编译指令、项目目录这些东西是**项目**的属性，
// 不是这台机器的属性。换个设备打开同一个项目，编译指令不该重新填一遍；
// 团队里别人也能在网页上直接看到这个项目怎么编译。
//
// 存法：状态为 `setting` 的任务，标题固定「项目设置」，正文（备注）是 TOML。
// 这样网页上直接可读可改，agent 也能 `tba setting` 读，GUI 也能编辑——
// 一份数据三处入口，不用同步。
//
// 本地缓存：data/projects/<id>/setting.toml。GUI 打开时先用缓存立刻渲染，
// 后台再拉云端覆盖；CLI 离线时也能用上一次的配置干活。

use crate::config::SETTING_STATE;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SETTING_TITLE: &str = "项目设置";

#[derive(Clone, Serialize, Deserialize)]
pub struct ProjectSetting {
    /// 这个项目要不要「编译 & 重载」。关掉后 agent 文案里不出现这段。
    #[serde(default = "default_on")]
    pub on: bool,
    /// 编译指令。空 = 还没填，让 agent 按项目类型自己探测后写回来。
    #[serde(default)]
    pub compile: String,
    /// 项目根目录（编译在这个目录里跑，exe 也从这里找）。
    #[serde(default)]
    pub root: String,
    /// exe 基名（不带 .exe）。
    #[serde(default)]
    pub name: String,
    /// rename = 改名直跑（默认）；copy = 副本运行。
    #[serde(default = "default_mode")]
    pub mode: String,
    /// copy 模式的运行副本路径。空 = 按 root/name 推算。
    #[serde(default)]
    pub live: String,
    /// 启动参数。
    #[serde(default)]
    pub args: String,
}

fn default_on() -> bool {
    true
}

fn default_mode() -> String {
    "rename".to_string()
}

impl Default for ProjectSetting {
    fn default() -> Self {
        ProjectSetting {
            on: true,
            compile: String::new(),
            root: String::new(),
            name: String::new(),
            mode: default_mode(),
            live: String::new(),
            args: String::new(),
        }
    }
}

impl ProjectSetting {
    /// 备注 → 结构体。
    ///
    /// 优先按 TOML 解析（正常情况）。解析不了也不报错：用户可能在网页上
    /// 顺手加了行说明、或者把 note 改成了 markdown，那就退化成逐行扫
    /// `key = value`，能捞多少捞多少——设置读不全比整个报错好。
    pub fn parse_note(note: &str) -> ProjectSetting {
        let text = note.trim_start_matches('\u{feff}');
        if let Ok(s) = toml::from_str::<ProjectSetting>(text) {
            return s;
        }
        let mut s = ProjectSetting::default();
        for line in text.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with('[') {
                continue;
            }
            let Some((k, v)) = t.split_once('=') else {
                continue;
            };
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            match k.trim() {
                "on" => s.on = v != "false",
                "compile" => s.compile = v,
                "root" => s.root = v,
                "name" => s.name = v,
                "mode" if !v.is_empty() => s.mode = v,
                "live" => s.live = v,
                "args" => s.args = v,
                _ => {}
            }
        }
        s
    }

    /// 结构体 → 备注。开头两行注释是给人看的：网页上打开这个任务，
    /// 一眼知道这堆 key 是干嘛的、改哪个字段。
    pub fn to_note(&self) -> String {
        let body = toml::to_string_pretty(self).unwrap_or_default();
        format!(
            "# tba 项目设置：窗口「编译 & 重载」/ `tba setting set` 都写这里。\n\
             # compile 留空 = 让 agent 按项目类型自己探测后填。\n\
             {body}"
        )
    }

    pub fn cache_path(store: &Store, project_id: &str) -> PathBuf {
        store.project_dir(project_id).join("setting.toml")
    }

    pub fn cache(&self, store: &Store, project_id: &str) -> Result<(), String> {
        let p = Self::cache_path(store, project_id);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("建目录失败: {e}"))?;
        }
        let s = toml::to_string_pretty(self).map_err(|e| format!("序列化失败: {e}"))?;
        std::fs::write(&p, s).map_err(|e| format!("写 {} 失败: {e}", p.display()))
    }

    pub fn load_cached(store: &Store, project_id: &str) -> Option<ProjectSetting> {
        let s = std::fs::read_to_string(Self::cache_path(store, project_id)).ok()?;
        toml::from_str(&s).ok()
    }

    /// 空字段按项目现状补默认值：根目录、exe 名、副本路径。
    /// 只补空的，用户填过的一律不动。
    pub fn fill_defaults(&mut self) {
        if self.root.trim().is_empty() {
            self.root = crate::capability::guess_root();
        }
        if self.name.trim().is_empty() {
            self.name = crate::capability::guess_name(&self.root);
        }
        if self.mode.trim().is_empty() {
            self.mode = default_mode();
        }
        if self.live.trim().is_empty() && self.mode == "copy" {
            self.live = crate::capability::default_live(&self.root, &self.name);
        }
        if self.compile.trim().is_empty() {
            self.compile = guess_compile(&self.root);
        }
    }

    /// 真正能干活的最小条件：有根目录、有 exe 名。
    pub fn ready(&self) -> bool {
        !self.root.trim().is_empty() && !self.name.trim().is_empty()
    }

    /// 文本能不能按 TOML 读。只用于 GUI 给提示：读不了也会走逐行扫描，
    /// 所以这里返回 false 不代表设置不可用。
    pub fn is_toml(text: &str) -> bool {
        toml::from_str::<ProjectSetting>(text.trim_start_matches('\u{feff}')).is_ok()
    }

    /// 明显会出问题的地方，给人看的短句。空 vec = 没发现毛病。
    /// 只报「按现在的值跑一定不对」的：宁可少报，也别把提示写成噪音。
    pub fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        if !self.mode.trim().is_empty() && self.mode.trim() != "rename" && self.mode.trim() != "copy"
        {
            v.push(format!(
                "mode 只能是 rename 或 copy，现在是 \"{}\"（会被当成 rename）",
                self.mode.trim()
            ));
        }
        if self.root.trim().is_empty() {
            v.push("root 空着：编译不知道在哪个目录跑".to_string());
        } else if !Path::new(self.root.trim()).is_dir() {
            v.push(format!("root 目录不存在：{}", self.root.trim()));
        }
        if self.name.trim().is_empty() {
            v.push("name 空着：找不到要启动的 exe".to_string());
        }
        v
    }

    /// 开工文案里「编译 & 重载」那一段。
    ///
    /// GUI 的「查看文案」和 `tba setting text` 都调这里，保证**看到的就是 agent 收到的**——
    /// 另写一份说明迟早会和实际文案对不上。
    pub fn prompt_block(&self, exe: &str, zh: bool) -> String {
        if !self.on {
            return String::new();
        }
        if self.compile.trim().is_empty() {
            return if zh {
                format!(
                    "编译 & 重载（本项目）：编译指令未填，先 `{exe} setting guess` 探测项目类型，\
                     再 `{exe} setting set --compile \"...\"` 写回 Teambition，然后 `{exe} reload build`\n\n"
                )
            } else {
                format!(
                    "Build & reload: compile not set. `{exe} setting guess` to detect the project type, \
                     then `{exe} setting set --compile \"...\"` (saved to Teambition), then `{exe} reload build`\n\n"
                )
            };
        }
        // 参数齐全时把关键项摆出来：agent 一眼能确认编译的是哪个项目、跑的是哪个 exe，
        // 不用再 `tba setting` 查一遍。
        let mut parts: Vec<String> = vec![format!(
            "{}: {}",
            if zh { "编译" } else { "compile" },
            self.compile.trim()
        )];
        if !self.root.trim().is_empty() {
            parts.push(format!(
                "{}: {}",
                if zh { "目录" } else { "dir" },
                self.root.trim()
            ));
        }
        if !self.name.trim().is_empty() {
            parts.push(format!("exe: {}", self.name.trim()));
        }
        let launch = if self.mode.trim() == "copy" {
            if zh { "副本运行" } else { "run a copy" }
        } else if zh {
            "直接跑产物"
        } else {
            "run in place"
        };
        parts.push(format!("{}: {launch}", if zh { "启动" } else { "launch" }));
        if zh {
            format!(
                "编译 & 重载（本项目）：`{exe} reload build`\n  {}\n  \
                 一条命令走完 编译 → 杀旧 → 启动，成功回 `OK <pid>`；失败看 `{exe} reload status`。\n\n",
                parts.join(" · ")
            )
        } else {
            format!(
                "Build & reload (this project): `{exe} reload build`\n  {}\n  \
                 One command: build -> kill old -> launch. `OK <pid>` on success; `{exe} reload status` on failure.\n\n",
                parts.join(" · ")
            )
        }
    }
}

/// 按项目里的构建文件猜编译指令。猜不到返回空串（让 agent 自己看）。
///
/// 只认「一眼能确定」的几种；猜错比空着更糟——空着 agent 会去读项目，
/// 猜错了它会照着跑，然后编译失败还得回头查。
pub fn guess_compile(root: &str) -> String {
    let p = Path::new(root);
    if p.join("Cargo.toml").exists() {
        return "cargo build --release".to_string();
    }
    if p.join("build.zig").exists() {
        return "zig build -Doptimize=ReleaseFast".to_string();
    }
    if p.join("CMakeLists.txt").exists() {
        return "cmake --build build --config Release".to_string();
    }
    if p.join("package.json").exists() {
        return "npm run build".to_string();
    }
    if p.join("Makefile").exists() || p.join("makefile").exists() {
        return "make".to_string();
    }
    if p.join("go.mod").exists() {
        return "go build ./...".to_string();
    }
    String::new()
}

/// 从 Teambition 读设置（不存在返回 None）。读完顺手更新本地缓存。
pub fn fetch(store: &Store, project_id: &str) -> Result<Option<ProjectSetting>, String> {
    // 离线：直接给本地缓存。返回 None 会让窗口提示「还没有设置」，
    // 而实际上本地可能有一份，只是没联网同步。
    if store.load_config().offline() {
        return Ok(ProjectSetting::load_cached(store, project_id));
    }
    let mut ctx = crate::ops::Ctx::new(store.clone(), false)?;
    let Some(state) = ctx.find_state(project_id, SETTING_STATE)? else {
        return Ok(None);
    };
    let tasks = ctx.list_tasks(project_id, &state)?;
    let Some(t) = tasks
        .iter()
        .find(|t| t.title == SETTING_TITLE)
        .or_else(|| tasks.first())
    else {
        return Ok(None);
    };
    let notes = crate::ops::read_task_notes(&mut ctx, &[t.id.clone()])?;
    let note = notes
        .first()
        .map(|(_, n)| n.clone())
        .unwrap_or_default();
    let s = ProjectSetting::parse_note(&note);
    let _ = s.cache(store, project_id);
    Ok(Some(s))
}

/// 写回 Teambition（没有就建）。写完更新本地缓存。
pub fn save(store: &Store, project_id: &str, s: &ProjectSetting) -> Result<String, String> {
    // 离线：只落本地缓存。设置本来就是「这台机器怎么编译这个项目」，
    // 断网时照样该能改——写云端等下次联网再说（不自动回写，避免把云端改成意外值）。
    if store.load_config().offline() {
        s.cache(store, project_id)?;
        return Ok(format!(
            "离线模式：已存到本地缓存（data/projects/{project_id}/setting.toml），没写云端"
        ));
    }
    let mut ctx = crate::ops::Ctx::new(store.clone(), false)?;
    let state = ctx.ensure_state(project_id, SETTING_STATE)?;
    let tasks = ctx.list_tasks(project_id, &state)?;
    let note = s.to_note();
    let msg = match tasks.iter().find(|t| t.title == SETTING_TITLE) {
        Some(t) => {
            ctx.update_task_content(&t.id, SETTING_TITLE, &note)?;
            format!("已保存到 Teambition（{}）", t.id)
        }
        None => {
            let id = ctx.create_marked_task(project_id, SETTING_TITLE, &note, &state)?;
            format!("已在 Teambition 建「{SETTING_TITLE}」任务并写入（{id}）")
        }
    };
    s.cache(store, project_id)?;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 开工文案里那段「编译 & 重载」：GUI 的「查看文案」和 `tba setting text` 读的就是它，
    // 所以它必须自己说得清「编译什么、在哪编、跑哪个 exe」，而不是让人再去 `tba setting` 查。
    #[test]
    fn prompt_block_carries_params() {
        let s = ProjectSetting {
            on: true,
            compile: "cargo build --release".into(),
            root: r"D:\dev\mcp".into(),
            name: "tba".into(),
            mode: "rename".into(),
            ..Default::default()
        };
        let zh = s.prompt_block("tba", true);
        assert!(zh.contains("编译 & 重载"), "{zh}");
        assert!(zh.contains("cargo build --release"), "要带编译指令: {zh}");
        assert!(zh.contains(r"D:\dev\mcp"), "要带项目目录: {zh}");
        assert!(zh.contains("exe: tba"), "要带 exe 名: {zh}");
        assert!(zh.contains("reload build"), "要给出跑法: {zh}");

        // 关掉的项目：这段整个不出现，别留一句「已停用」白占字数。
        let off = ProjectSetting {
            on: false,
            ..s.clone()
        };
        assert!(off.prompt_block("tba", true).is_empty());

        // 编译指令空着：给探测入口，而不是默认按 cargo 硬编。
        let empty = ProjectSetting {
            compile: String::new(),
            ..s
        };
        let e = empty.prompt_block("tba", true);
        assert!(e.contains("setting guess"), "空指令要给探测入口: {e}");
        assert!(!e.contains("cargo build"), "不该替用户猜指令: {e}");
    }

    #[test]
    fn note_roundtrip() {        let s = ProjectSetting {
            on: false,
            compile: "zig build -Doptimize=ReleaseFast".to_string(),
            root: r"D:\dev\zigapp".to_string(),
            name: "zigapp".to_string(),
            mode: "copy".to_string(),
            live: r"D:\dev\zigapp\zig-out\bin\zigapp_test.exe".to_string(),
            args: "--debug".to_string(),
        };
        let back = ProjectSetting::parse_note(&s.to_note());
        assert!(!back.on);
        assert_eq!(back.compile, s.compile);
        assert_eq!(back.root, s.root, "反斜杠要原样回来，不能被吃掉");
        assert_eq!(back.mode, "copy");
        assert_eq!(back.live, s.live);
        assert_eq!(back.args, "--debug");
    }

    // 用户在网页上把备注改了（加了说明、少了几个字段）也不能整个读崩。
    #[test]
    fn note_falls_back_to_line_scan() {
        let note = r#"这个项目怎么编译：
compile = "cargo build --release"
root = 'D:\dev\mcp'
乱写一行
"#;
        let s = ProjectSetting::parse_note(note);
        assert_eq!(s.compile, "cargo build --release");
        assert_eq!(s.root, r"D:\dev\mcp");
        assert!(s.on, "缺字段用默认值（开）");
        assert_eq!(s.mode, "rename");
    }

    #[test]
    fn guess_compile_by_build_file() {        let d = std::env::temp_dir().join(format!("tba-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(guess_compile(&d.display().to_string()), "");

        std::fs::write(d.join("build.zig"), "// zig").unwrap();
        assert_eq!(
            guess_compile(&d.display().to_string()),
            "zig build -Doptimize=ReleaseFast"
        );
        std::fs::remove_file(d.join("build.zig")).unwrap();
        std::fs::write(d.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(
            guess_compile(&d.display().to_string()),
            "cargo build --release"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    // GUI 现在直接编辑 TOML 文本，这两个函数决定给什么提示。
    #[test]
    fn problems_flag_bad_mode_and_missing_root() {
        let bad = ProjectSetting {
            mode: "cpy".to_string(),
            ..Default::default()
        };
        let p = bad.problems();
        assert!(p.iter().any(|x| x.contains("mode")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("root")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("name")), "{p:?}");

        let ok = ProjectSetting {
            compile: "cargo build".to_string(),
            root: std::env::temp_dir().display().to_string(),
            name: "tba".to_string(),
            mode: "rename".to_string(),
            ..Default::default()
        };
        assert!(ok.problems().is_empty(), "{:?}", ok.problems());
    }

    #[test]
    fn is_toml_detects_bad_quoting() {
        assert!(ProjectSetting::is_toml("compile = \"cargo build\"\n"));
        // Windows 路径写进双引号里，\d 是非法转义：TOML 读不了，
        // 但逐行扫描能原样捞回来——所以「读不了」只是提示，不是错误。
        assert!(!ProjectSetting::is_toml("root = \"D:\\dev\\mcp\"\n"));
        assert_eq!(
            ProjectSetting::parse_note("root = \"D:\\dev\\mcp\"\n").root,
            r"D:\dev\mcp"
        );
    }
}
