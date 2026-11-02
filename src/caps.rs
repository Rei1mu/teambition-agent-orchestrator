// 通用能力（cap）：本地、跨项目的文本能力库。
//
// 和 Teambition 项目 skill 的区别（这两件事以前混在一起，导致「截图」这种
// 跟项目无关的能力被推进了某个项目的云端 skill 里，换个项目就没了）：
//   - 项目 skill：这个项目怎么干活，存 Teambition 云端，按项目 id 取；
//   - 通用能力：这台机器上的 tba 能干什么，存本地 data/caps.toml，所有项目共用。
//
// 每条能力 = 名字 + 一句话说明 + 开关 + 正文（markdown）。
// agent 用 `tba cap list` 看目录、`tba cap <名字>` 读正文——按需读，省 token。
// GUI 的「通用能力 → 能力」页可以直接增删改查，改完立刻落盘，不用联网。
//
// 为什么正文是文本而不是写死在代码里：命令（tba shot / reload / publish）负责
// 「怎么做」的确定性，正文只写「什么时候用、参数、红线」。后者会随使用习惯变，
// 让用户自己改比每次发版改代码划算。

use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CAP_FILE: &str = "caps.toml";

#[derive(Clone, Serialize, Deserialize)]
pub struct Cap {
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default = "default_on")]
    pub on: bool,
    /// 内置条目：正文可改、可删，但删了能用 `tba cap restore` 找回来。
    #[serde(default)]
    pub builtin: bool,
    #[serde(default)]
    pub body: String,
}

fn default_on() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct CapsFile {
    #[serde(default)]
    pub caps: Vec<Cap>,
}

// ---- 内置正文 ----
//
// 只写「什么时候用、参数、红线」，实现细节在命令里。改这里等于改默认值，
// 用户已经改过的条目不会被覆盖（load 只在文件不存在时灌一次）。

pub const BODY_SHOT: &str = "\
# 截图

## 一条命令
`tba shot --out .tmp/s.png`

## 参数
- `--out <路径>`：默认 `shot.png`。目录不存在会自动建。
- `--title <标题片段>`：截指定窗口，标题包含匹配，取第一个可见窗口。
- `--hwnd <句柄>`：直接给句柄，优先于 `--title`。
- `--level fast|balanced|high`：压缩级别，**都是无损**。默认 fast（毫秒级，全屏约 0.3–0.8 MB）；嫌大用 high。

## 要点
- 用 BitBlt 抓屏，**不需要把窗口提到前台**，后台窗口也能截。
- **不要自己写 PowerShell 截图脚本**，也不要找第三方工具。就这一条命令。
- 截完用图片查看工具看 `.tmp/s.png`，不要凭想象描述界面。
";

pub const BODY_GIT: &str = "\
# 提交并推送

## 先看状态
`tba git status` —— 分支、脏文件、远端领先/落后、是不是 GitButler 项目。

## 一步提交推送
`tba git main -m \"feat: 改了什么\"`

- GitButler 项目自动走 `but commit` + `but push`；普通仓库走 `git add -A` + `commit` + `push origin HEAD:<分支>`。
- 默认推到 origin/HEAD 指向的分支（通常是 main）。要指定加 `--branch`。
- GitButler 里要直接落到目标分支：加 `--land`。
- 只想看会跑什么：`--dry`。
- 只打 tag：`tba git tag --tag v1.2.3`

## 红线
- 不 force push。push 被拒先 `git pull --rebase`。
- 不提交 `.env`、token、`data/`。
- 提交信息写清「改了什么」，别用「update」。
";

pub const BODY_RELEASE: &str = "\
# 发版（GitHub Actions）

## 0. 仓库还没有 release 流程
`tba publish --init`

生成 `.github/workflows/release.yml`：windows x64 + windows arm64（MSVC 交叉编译）
两个 job，有 `installer/*.iss` 就出 setup.exe，没有就出 zip，tag 推上去自动发 Release。
已有同名文件且内容不同时会拒绝，确认覆盖加 `--force`。

## 1. 升版本号
改 `Cargo.toml` 的 `version`，**然后跑一次 `cargo build` 让 Cargo.lock 跟上**，
有 CHANGELOG 就加一条，然后提交。
（CI 用 `--locked`，lock 里还是旧版本号会直接失败：`cannot update the lock file ... --locked`）

## 2. 一条命令发版
`tba publish --tag vX.Y.Z --watch`

打 tag -> 推 tag -> 等 CI -> 列产物。没有 `--tag` 时读 Cargo.toml 的 version。
`--watch` 需要 GitHub token（`GH_TOKEN` 环境变量，或 git 凭据里存过）；
拿不到就只打 tag，自己开 Actions 页看。

## 3. 收尾
CI 绿了再给用户地址：`https://github.com/<owner>/<repo>/releases/tag/vX.Y.Z`

## 红线
- CI 没绿不许说「已发布」。
- 版本号只升不降；已有 tag 默认拒绝重打。
- 打包脚本改了，先本地打一次包再打 tag。
";

pub const BODY_GUI: &str = "\
# 写 GUI 的注意事项

## 字体
- **中文用黑体**：SimHei（simhei.ttf）或 Microsoft YaHei（msyh.ttc）。别用默认字体，中文会掉成方框。
- 字体链要有回退：首选 -> 备用中文字体 -> 符号字体（seguisym.ttf）。只装一个字体必然缺字形。
- 字体要**可配置**：设置里列本机已装字体让用户选，选不到就回落链首。

## 别出豆腐块（口）
- 符号（⚠ ✓ ▾ ⚙ emoji）先确认目标字体有字形，没有就换成文字，比如「注意：」「设置」。
- 上线前把所有符号渲染一遍看结果，别只看代码。

## 控件
- 字号可调，默认 18px。数字用可左右拖的 DragValue，**不要滑块**，也不要只能打字的输入框。
- 同一区域按钮尺寸统一，`button` 和 `small_button` 别混用。
- 内容超出窗口要能滚动，底部按钮不能被裁。

## 其它
- 启动声明 DPI 感知，否则 125%/150% 缩放下整个界面糊。
- 打包成 GUI 程序别弹控制台黑窗（Windows 上 `FreeConsole`，别改 subsystem，会吞掉 CLI 输出）。

## 验证
改完必须 `tba shot --title <窗口标题> --out .tmp/ui.png` 截图自己看，不要凭想象说「已修好」。
";

pub const BODY_RELOAD: &str = "\
# 重载（无痛重启）

## 一条命令
`tba reload build`

编译 -> 杀旧 -> 复制校验 -> 启动，四步都在命令里，退出码是唯一真值：
0 成功 / 2 超时 / 3 别处已收尾 / 4 项目不存在 / 5 BLOCKED。

## 动作
- `tba reload build`：编译 + 替换 + 启动（最常用）
- `tba reload restart`：不编译，直接杀旧 + 启动
- `tba reload kill`：只杀
- `tba reload status`：看目标进程在不在

## 参数
`--root <项目根>` / `--name <exe 基名>` / `--live <运行副本路径>` /
`--mode rename|copy` / `--elevate` / `--args <启动参数>`。缺省按 exe 位置和 Cargo.toml 推断。

## 红线
- 不要自己 kill + copy + start，也不要擅自把 exe 改名成 `_new_text01.exe` 之类。
- 编译失败时旧窗口不动、继续可用；不要手动删 target 或清缓存重来。
- 重载 tba 自己时命令会自动转后台（等本进程退出再编译），不要在前台等。
- 报 BLOCKED 就是别人正在重载，等它收尾，别抢。
";

// (名字, 说明, 正文)
pub const BUILTIN: &[(&str, &str, &str)] = &[
    ("reload", "重载：编译 -> 杀旧 -> 复制校验 -> 启动", BODY_RELOAD),
    ("shot", "截图：一条命令出 png，后台窗口也能截", BODY_SHOT),
    ("git-main", "提交并推送（GitButler 项目自动走 but）", BODY_GIT),
    (
        "release",
        "发版：生成 workflow / 打 tag / 等 CI / 列产物",
        BODY_RELEASE,
    ),
    (
        "gui-notes",
        "写 GUI 的注意事项：黑体、字体可选、别出豆腐块",
        BODY_GUI,
    ),
];

pub fn path(store: &Store) -> PathBuf {
    store.root.join(CAP_FILE)
}

fn builtin_caps() -> Vec<Cap> {
    BUILTIN
        .iter()
        .map(|(name, summary, body)| Cap {
            name: (*name).to_string(),
            summary: (*summary).to_string(),
            on: true,
            builtin: true,
            body: (*body).to_string(),
        })
        .collect()
}

// 读全部能力。文件不存在 = 首次使用，灌一份内置模板并落盘。
// 文件存在但是空数组时**不重灌**——那是用户自己删光的，别跟他抢。
pub fn load(store: &Store) -> CapsFile {
    let p = path(store);
    match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => {
            let f = CapsFile {
                caps: builtin_caps(),
            };
            let _ = save(store, &f);
            f
        }
    }
}

pub fn save(store: &Store, f: &CapsFile) -> Result<(), String> {
    let p = path(store);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {e}"))?;
    }
    let s = toml::to_string_pretty(f).map_err(|e| format!("序列化失败: {e}"))?;
    // 先写临时文件再改名：写一半崩了不会留下半截 caps.toml。
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, s).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("替换 {} 失败: {e}", p.display()))?;
    Ok(())
}

pub fn list(store: &Store) -> Vec<Cap> {
    load(store).caps
}

pub fn get(store: &Store, name: &str) -> Option<Cap> {
    load(store).caps.into_iter().find(|c| c.name == name)
}

// 只给启用的，给 agent 文案用。
pub fn enabled(store: &Store) -> Vec<Cap> {
    load(store)
        .caps
        .into_iter()
        .filter(|c| c.on)
        .collect()
}

// 名字是命令行参数，也是文件名之外的唯一键，限制字符集省得 agent 传奇怪的串。
pub fn check_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("名字不能为空".to_string());
    }
    if n.chars().count() > 40 {
        return Err("名字太长（最多 40 字符）".to_string());
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("名字只能用字母、数字、- 和 _：{n}"));
    }
    Ok(())
}

// 新建或覆盖。同名视为编辑，保留原来的 on 开关和 builtin 标记。
pub fn upsert(store: &Store, mut cap: Cap) -> Result<String, String> {
    check_name(&cap.name)?;
    cap.name = cap.name.trim().to_string();
    let mut f = load(store);
    match f.caps.iter_mut().find(|c| c.name == cap.name) {
        Some(old) => {
            old.summary = cap.summary;
            old.body = cap.body;
            // 开关以传入值为准：GUI 编辑时会带上原值，CLI 的 --off 也能直接生效。
            old.on = cap.on;
            // 内置标记只增不减：用户手改过正文的 shot 仍然是内置条目，还能 restore。
            if cap.builtin {
                old.builtin = true;
            }
            save(store, &f)?;
            Ok(format!("已更新能力: {}", cap.name))
        }
        None => {
            let name = cap.name.clone();
            f.caps.push(cap);
            save(store, &f)?;
            Ok(format!("已新增能力: {name}"))
        }
    }
}

pub fn set_on(store: &Store, name: &str, on: bool) -> Result<String, String> {
    let mut f = load(store);
    let c = f
        .caps
        .iter_mut()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("没有这个能力: {name}（tba cap list 看有哪些）"))?;
    c.on = on;
    save(store, &f)?;
    Ok(format!(
        "{}: {name}",
        if on { "已启用" } else { "已关闭" }
    ))
}

pub fn remove(store: &Store, name: &str) -> Result<String, String> {
    let mut f = load(store);
    let before = f.caps.len();
    f.caps.retain(|c| c.name != name);
    if f.caps.len() == before {
        return Err(format!("没有这个能力: {name}"));
    }
    save(store, &f)?;
    let recover = BUILTIN.iter().any(|(n, _, _)| *n == name);
    Ok(if recover {
        format!("已删除能力: {name}（内置条目，`tba cap restore {name}` 可恢复）")
    } else {
        format!("已删除能力: {name}")
    })
}

// 从内置模板恢复。给名字只恢复那一条，不给则补齐所有缺失的内置条目。
pub fn restore(store: &Store, name: Option<&str>) -> Result<String, String> {
    let mut f = load(store);
    let mut done = Vec::new();
    for (n, summary, body) in BUILTIN {
        if let Some(want) = name {
            if want != *n {
                continue;
            }
        }
        match f.caps.iter_mut().find(|c| c.name == *n) {
            Some(c) => {
                // 已存在：只把正文拉回默认，开关和说明留着用户改过的样子。
                c.body = (*body).to_string();
                if c.summary.is_empty() {
                    c.summary = (*summary).to_string();
                }
                c.builtin = true;
                done.push(format!("已重置 {n}"));
            }
            None => {
                f.caps.push(Cap {
                    name: (*n).to_string(),
                    summary: (*summary).to_string(),
                    on: true,
                    builtin: true,
                    body: (*body).to_string(),
                });
                done.push(format!("已恢复 {n}"));
            }
        }
    }
    if done.is_empty() {
        return Err(format!(
            "没有这个内置能力: {}（可选 {}）",
            name.unwrap_or(""),
            BUILTIN
                .iter()
                .map(|(n, _, _)| *n)
                .collect::<Vec<_>>()
                .join(" / ")
        ));
    }
    save(store, &f)?;
    Ok(done.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("tba-cap-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Store { root }
    }

    // 首次使用自动灌内置模板，且立刻落盘（下次不会重灌、不会覆盖用户改动）。
    #[test]
    fn first_load_seeds_builtins() {
        let store = tmp("seed");
        let caps = list(&store);
        assert_eq!(caps.len(), BUILTIN.len());
        assert!(path(&store).exists());
        assert!(caps.iter().any(|c| c.name == "reload"));
        assert!(caps.iter().all(|c| c.on && c.builtin));
    }

    // 用户把能力删光 = 空数组，不能当成「没初始化」再灌回来。
    #[test]
    fn empty_file_is_not_reseeded() {
        let store = tmp("empty");
        save(
            &store,
            &CapsFile {
                caps: Vec::new(),
            },
        )
        .unwrap();
        assert!(list(&store).is_empty());
    }

    // 增删改查一轮：改名不合法要拒，删了能恢复。
    #[test]
    fn crud_roundtrip() {
        let store = tmp("crud");
        assert!(check_name("bad name").is_err());
        assert!(check_name("").is_err());
        upsert(
            &store,
            Cap {
                name: "my-cap".into(),
                summary: "自用".into(),
                on: true,
                builtin: false,
                body: "# 正文".into(),
            },
        )
        .unwrap();
        let c = get(&store, "my-cap").expect("刚加的能力应该在");
        assert_eq!(c.body, "# 正文");
        assert!(!c.builtin);

        set_on(&store, "my-cap", false).unwrap();
        assert!(!get(&store, "my-cap").unwrap().on);
        assert_eq!(enabled(&store).iter().filter(|c| c.name == "my-cap").count(), 0);

        remove(&store, "my-cap").unwrap();
        assert!(get(&store, "my-cap").is_none());

        // 内置条目删掉后 restore 能找回来，正文是模板原文
        remove(&store, "shot").unwrap();
        assert!(get(&store, "shot").is_none());
        restore(&store, Some("shot")).unwrap();
        assert_eq!(get(&store, "shot").unwrap().body, BODY_SHOT);
    }

    // 编辑同名条目不该产生第二条；开关以传入值为准（GUI 编辑时带原值，不会误开）。
    #[test]
    fn upsert_same_name_updates_in_place() {
        let store = tmp("upsert");
        set_on(&store, "shot", false).unwrap();
        upsert(
            &store,
            Cap {
                name: "shot".into(),
                summary: "改过的说明".into(),
                on: true,
                builtin: false,
                body: "新正文".into(),
            },
        )
        .unwrap();
        let all = list(&store);
        assert_eq!(all.iter().filter(|c| c.name == "shot").count(), 1);
        let c = get(&store, "shot").unwrap();
        assert_eq!(c.body, "新正文");
        assert_eq!(c.summary, "改过的说明");
        assert!(c.on, "开关以传入值为准：显式传 true 就该打开");
        assert!(c.builtin, "内置标记不该被覆盖丢掉");
    }
}
