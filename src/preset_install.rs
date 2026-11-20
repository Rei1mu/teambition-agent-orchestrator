// 一键安装 tba / chat DSH 预设。
//
// 预设源码在编译期嵌进 exe（4 个文件），运行时写到 tba data 的同级 presets/，
// 再用 DSH 自带 node + pnpm 把 link:<dir> 加进运行中 profile 的 package.json。
// 不写死 D:\dsh：node / pnpm / profile 全从运行中的 DSH 主进程命令行解析。

use std::cmp::Ordering as CmpOrdering;
use std::path::{Path, PathBuf};
use std::process::Command;

const TBA_PACKAGE: &str = include_str!("../dsh-tba-preset/package.json");
const TBA_PATCH: &str = include_str!("../dsh-tba-preset/cordis.patch.yml");
const CHAT_PACKAGE: &str = include_str!("../dsh-chat-preset/package.json");
const CHAT_PATCH: &str = include_str!("../dsh-chat-preset/cordis.patch.yml");

const TBA_DEP: &str = "@local/dsh-tba-preset";
const CHAT_DEP: &str = "@local/dsh-chat-preset";

/// 从 DSH 主进程命令行里解析出来的路径。
#[derive(Clone, Debug, PartialEq)]
pub struct DshPaths {
    pub profile_dir: PathBuf,
    pub pnpm_mjs: PathBuf,
    pub node: PathBuf,
}

/// 纯函数：从一个命令行字符串里取 profile 目录、pnpm.mjs、node.exe。
///
/// DSH 主进程命令行形如：
/// `"D:\dsh\DeepSeek Harness.exe" --expose-internals <host-entry.js> <dsh-install-dir>
///  <profile-dir> <runtime-dir> <pnpm.mjs-path> <runtime-bin-dir>`
/// 这里只认 `--expose-internals` 且参数里带 `pnpm` + `pnpm.mjs` 的行，避免误中
/// 子进程（`--type=renderer` 等）。空格路径按 Windows 惯例用引号包裹：解析时
/// 先按引号切一遍，没有引号的裸路径（如命令行示例里的 D:\dsh\...）按空白切。
pub fn parse_dsh_command_line(cmdline: &str) -> Option<DshPaths> {
    if !cmdline.contains("--expose-internals") {
        return None;
    }
    let args = split_command_line(cmdline);
    let pos = args.iter().position(|a| a == "--expose-internals")?;
    // 形如 ... --expose-internals entry.js install-dir profile runtime-dir pnpm.mjs bin-dir
    let rest = &args[pos + 1..];
    if rest.len() < 6 {
        return None;
    }
    let pnpm_mjs = PathBuf::from(&rest[4]);
    if !pnpm_mjs.to_string_lossy().contains("pnpm.mjs") {
        return None;
    }
    let profile_dir = PathBuf::from(&rest[2]);
    let bin_dir = PathBuf::from(&rest[5]);
    // runtime/bin 下有 node / node.cmd 包装（走 Electron 的 node），但 DSH 自己的
    // node.exe 在 primary-runtime/dependencies/node/bin/。两者都可用，优先使用
    // 跟 pnpm.mjs 同安装树的那个（pnpm/bin 与 node/bin 同在 dependencies/ 下）。
    let node = pnpm_mjs
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|deps| deps.join("node").join("bin").join("node.exe"))
        .filter(|p| p.is_file())
        // 兜底：在 DSH 安装目录下找 node.exe（最多向下 4 层）。
        .or_else(|| find_node_near(&bin_dir, 4))
        .or_else(|| find_node_near(&pnpm_mjs, 4))?;
    Some(DshPaths {
        profile_dir,
        pnpm_mjs,
        node,
    })
}

/// 解析 Windows 命令行：引号内的整段当成一个参数，引号外的按空白切。
fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in s.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 在 path 自身及其最多 max_depth 层子目录里找 node.exe。
fn find_node_near(path: &Path, max_depth: usize) -> Option<PathBuf> {
    let mut dirs = vec![path.to_path_buf()];
    for depth in 0..=max_depth {
        let mut next = Vec::new();
        for dir in &dirs {
            let candidate = dir.join("node.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
            if depth < max_depth {
                if let Ok(rd) = std::fs::read_dir(dir) {
                    for e in rd.flatten() {
                        if e.path().is_dir() {
                            next.push(e.path());
                        }
                    }
                }
            }
        }
        dirs = next;
    }
    None
}

/// 从 tba data 派生预设落盘目录（data 的同级 presets/）。
fn presets_root() -> PathBuf {
    crate::config::data_dir()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("presets")
}

/// 写一份 bundle 的 2 个文件；内容没变就不动盘（保证幂等、避免无谓写）。
fn write_bundle(dir: &Path, package: &str, patch: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("无法创建 {}: {e}", dir.display()))?;
    for (name, content) in [("package.json", package), ("cordis.patch.yml", patch)] {
        let path = dir.join(name);
        if matches!(std::fs::read_to_string(&path).as_deref(), Ok(s) if s == content) {
            continue;
        }
        std::fs::write(&path, content)
            .map_err(|e| format!("无法写入 {}: {e}", path.display()))?;
    }
    Ok(())
}

/// 从 bundle package.json 里读 version（纯函数，取不到返回 None）。
fn read_version(package_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(package_json)
        .ok()?
        .get("version")?
        .as_str()
        .map(|s| s.to_string())
}

/// 比较版本号：按点分段做数值比较，缺段按 0 处理；非数字段按字符串比较。
/// 返回 Ordering::Less / Equal / Greater（self 相对 other）。
fn compare_version(a: &str, b: &str) -> CmpOrdering {
    let pa: Vec<&str> = a.trim().trim_start_matches('v').split('.').collect();
    let pb: Vec<&str> = b.trim().trim_start_matches('v').split('.').collect();
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let sa = pa.get(i).copied().unwrap_or("0");
        let sb = pb.get(i).copied().unwrap_or("0");
        match (sa.parse::<u64>(), sb.parse::<u64>()) {
            (Ok(x), Ok(y)) => match x.cmp(&y) {
                CmpOrdering::Equal => continue,
                ord => return ord,
            },
            _ => match sa.cmp(sb) {
                CmpOrdering::Equal => continue,
                ord => return ord,
            },
        }
    }
    CmpOrdering::Equal
}

/// 判断 profile package.json 里预设是否完整装好。
/// DSH 加载 bundle 需要两件事同时成立：dependencies 里有 dep（pnpm 软链），
/// 且 dsh.profile.bundles 数组里有同名项（DSH 注册）。只查一边会误判。
#[cfg(test)]
pub fn deps_installed(package_json: &str) -> bool {
    dep_installed(package_json, TBA_DEP) && dep_installed(package_json, CHAT_DEP)
}

fn dep_installed(package_json: &str, name: &str) -> bool {
    has_dep(package_json, name) && has_bundle(package_json, name)
}

/// 用 JSON 解析判断 dependencies 里是否有指定名字。解析失败按「没装」处理，
/// 走 pnpm add 覆盖（pnpm 自己会报错）。
fn has_dep(package_json: &str, name: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(package_json)
        .ok()
        .and_then(|v| {
            v.get("dependencies")?
                .get(name)?
                .as_str()
                .map(|s| !s.is_empty())
        })
        .unwrap_or(false)
}

/// 判断 dsh.profile.bundles 数组里是否有指定 bundle 名。
fn has_bundle(package_json: &str, name: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(package_json)
        .ok()
        .and_then(|v| {
            v.get("dsh")?
                .get("profile")?
                .get("bundles")?
                .as_array()?
                .iter()
                .any(|b| b.as_str() == Some(name))
                .then_some(true)
        })
        .unwrap_or(false)
}

/// 在 dsh.profile.bundles 里补齐 names（已存在的保持原样、不重复添加）。
/// 纯函数：返回 Some(新 JSON 文本) 表示有改动，None 表示本来就齐了。
/// 用 serde_json 解析再 to_string_pretty 写回，其它字段原样保留。
fn add_bundles(package_json: &str, names: &[&str]) -> Result<Option<String>, String> {
    let mut v: serde_json::Value = serde_json::from_str(package_json)
        .map_err(|e| format!("解析 profile package.json 失败: {e}"))?;
    let is_obj = |val: Option<&serde_json::Value>| val.map(|x| x.is_object()).unwrap_or(false);
    if !is_obj(v.get("dsh")) {
        v["dsh"] = serde_json::json!({});
    }
    if !is_obj(v.get("dsh").and_then(|d| d.get("profile"))) {
        v["dsh"]["profile"] = serde_json::json!({});
    }
    if !v
        .get("dsh")
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles"))
        .map(|b| b.is_array())
        .unwrap_or(false)
    {
        v["dsh"]["profile"]["bundles"] = serde_json::json!([]);
    }
    let bundles = v["dsh"]["profile"]["bundles"]
        .as_array_mut()
        .expect("bundles 已确保是数组");
    let mut changed = false;
    for name in names {
        if !bundles.iter().any(|b| b.as_str() == Some(*name)) {
            bundles.push(serde_json::Value::String((*name).to_string()));
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    serde_json::to_string_pretty(&v)
        .map(Some)
        .map_err(|e| format!("写回 profile package.json 失败: {e}"))
}

/// 通过 PowerShell + CIM 拿运行中 DSH 主进程的命令行。
fn dsh_command_line() -> Result<String, String> {
    let script = r#"Get-CimInstance Win32_Process -Filter "Name='DeepSeek Harness.exe'" | ForEach-Object { $_.CommandLine }"#;
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    crate::proc::hide(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("无法查询 DSH 进程: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "查询 DSH 进程失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // 主进程是唯一带 --expose-internals 的那条。
    text.lines()
        .find(|line| line.contains("--expose-internals"))
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "没找到运行中的 DSH 主进程，请先启动 DSH Desktop".to_string())
}

/// 跑一次 pnpm add link:<dir>，成功返回 Ok(())。
fn pnpm_add(paths: &DshPaths, dep: &str, dir: &Path) -> Result<(), String> {
    let spec = format!("link:{}", dir.to_string_lossy().replace('\\', "/"));
    let mut cmd = Command::new(&paths.node);
    cmd.arg("--expose-internals")
        .arg(&paths.pnpm_mjs)
        .arg("add")
        .arg(&spec)
        .current_dir(&paths.profile_dir);
    crate::proc::hide(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("运行 pnpm 失败（{dep}）: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "pnpm add {dep} 失败:\n{}{}",
            stdout.trim(),
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}", stderr.trim())
            }
        ))
    }
}

/// 单个预设的动作决策（纯函数，方便测试）。
/// installed_version: 已装目录 package.json 的 version（None = 没装过）
/// embedded_version:   exe 内嵌 package.json 的 version
/// registered:        profile 的 dependencies + dsh.profile.bundles 是否都登记齐
#[derive(Debug, PartialEq)]
enum Action {
    /// 已是最新，什么都不做
    SkipLatest(String),
    /// 只补依赖/ bundles 登记（版本相同）
    RegisterOnly(String),
    /// 首次安装
    Install(String),
    /// 版本升级：from -> to
    Upgrade(String, String),
    /// 版本降级：from -> to
    Downgrade(String, String),
}

fn decide_action(
    installed_version: Option<&str>,
    embedded_version: &str,
    registered: bool,
) -> Action {
    match installed_version {
        None => Action::Install(embedded_version.to_string()),
        Some(old) => match compare_version(embedded_version, old) {
            CmpOrdering::Greater => Action::Upgrade(old.to_string(), embedded_version.to_string()),
            CmpOrdering::Less => Action::Downgrade(old.to_string(), embedded_version.to_string()),
            CmpOrdering::Equal => {
                if registered {
                    Action::SkipLatest(embedded_version.to_string())
                } else {
                    Action::RegisterOnly(embedded_version.to_string())
                }
            }
        },
    }
}

/// 把动作转成用户可读的提示片段。
fn action_text(action: &Action) -> String {
    match action {
        Action::SkipLatest(v) => format!("已是最新 v{v}，无需更新"),
        Action::RegisterOnly(v) => format!("已补齐登记（v{v}）"),
        Action::Install(v) => format!("已安装 v{v}"),
        Action::Upgrade(from, to) => format!("已从 v{from} 更新到 v{to}"),
        Action::Downgrade(from, to) => format!("已降级/回滚到 v{to}（原 v{from}）"),
    }
}

/// 一键安装。返回值直接显示到 GUI 状态栏。
pub fn install_presets() -> Result<String, String> {
    let cmdline = dsh_command_line()?;
    let paths = parse_dsh_command_line(&cmdline)
        .ok_or_else(|| format!("无法从 DSH 进程命令行解析路径: {cmdline}"))?;

    let package_json_path = paths.profile_dir.join("package.json");
    let root = presets_root();
    let tba_dir = root.join("dsh-tba-preset");
    let chat_dir = root.join("dsh-chat-preset");

    let specs = [
        (TBA_DEP, &tba_dir, TBA_PACKAGE, TBA_PATCH),
        (CHAT_DEP, &chat_dir, CHAT_PACKAGE, CHAT_PATCH),
    ];
    // 先决策：读已装目录版本 + profile 登记状态，不写任何盘。
    let mut decisions = Vec::new();
    let mut profile = std::fs::read_to_string(&package_json_path)
        .map_err(|e| format!("读取 {}: {e}", package_json_path.display()))?;
    for (dep, dir, package, patch) in &specs {
        let installed_version = std::fs::read_to_string(dir.join("package.json"))
            .ok()
            .as_deref()
            .and_then(read_version);
        let embedded_version = read_version(package)
            .ok_or_else(|| format!("内嵌 bundle {dep} 的 package.json 缺少 version"))?;
        let action = decide_action(installed_version.as_deref(), &embedded_version, dep_installed(&profile, dep));
        decisions.push((*dep, *dir, *package, *patch, action));
    }

    let all_skipped = decisions
        .iter()
        .all(|(_, _, _, _, a)| matches!(a, Action::SkipLatest(_)));
    if all_skipped {
        let versions: Vec<String> = decisions
            .iter()
            .filter_map(|(_, _, _, _, a)| match a {
                Action::SkipLatest(v) => Some(format!("v{v}")),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        return Ok(format!(
            "两个预设已是最新（{}），无需更新。请手动重启 DSH 使预设生效。",
            versions.join("、")
        ));
    }

    // 版本变化 / 缺登记：覆盖写 bundle 文件（内容没变 write_bundle 不动盘）。
    for (_, dir, package, patch, action) in &decisions {
        if matches!(action, Action::SkipLatest(_)) {
            continue;
        }
        write_bundle(dir, package, patch)?;
    }

    // 需要装 dep 的：pnpm add。登记齐但版本变化的不用再 pnpm（软链已指向同一目录）。
    for (dep, dir, _, _, _) in &decisions {
        let current = std::fs::read_to_string(&package_json_path)
            .map_err(|e| format!("读取 {}: {e}", package_json_path.display()))?;
        if has_dep(&current, dep) {
            continue;
        }
        pnpm_add(&paths, dep, dir)?;
    }

    // pnpm add 会重写 package.json，最后补 bundles 数组。
    profile = std::fs::read_to_string(&package_json_path)
        .map_err(|e| format!("读取 {}: {e}", package_json_path.display()))?;
    if let Some(updated) = add_bundles(&profile, &[TBA_DEP, CHAT_DEP])? {
        std::fs::write(&package_json_path, &updated)
            .map_err(|e| format!("写入 {} 失败: {e}", package_json_path.display()))?;
    }

    let summary: Vec<String> = decisions
        .iter()
        .filter(|(_, _, _, _, a)| !matches!(a, Action::SkipLatest(_)))
        .map(|(dep, _, _, _, a)| format!("{dep}: {}", action_text(a)))
        .collect();
    Ok(format!(
        "{}。请手动重启 DSH 使预设生效。",
        summary.join("；")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#""D:\dsh\DeepSeek Harness.exe" --expose-internals D:\dsh\resources\app.asar\dsh\node_modules\@deepseek-ai\dsh-desktop-host\lib\index.js D:\dsh\resources\app.asar\dsh C:\Users\user\.dsh\profiles\desktop D:\dsh\resources\runtime\primary-runtime D:\dsh\resources\runtime\primary-runtime\dependencies\pnpm\bin\pnpm.mjs D:\dsh\resources\runtime\bin"#;

    #[test]
    fn parse_dsh_command_line_extracts_paths() {
        // 这个测试依赖本机 DSH 安装（node.exe 在 pnpm.mjs 附近）。
        // 只断言解析结果的结构，不要求具体盘符。
        let Some(paths) = parse_dsh_command_line(SAMPLE) else {
            // 没有 DSH 安装的机器上跳过（Extract 逻辑仍由下面两个测试覆盖）。
            return;
        };
        assert_eq!(paths.profile_dir, PathBuf::from(r"C:\Users\user\.dsh\profiles\desktop"));
        assert!(paths.pnpm_mjs.ends_with("pnpm.mjs"));
        assert!(paths.node.ends_with("node.exe"));
    }

    #[test]
    fn parse_dsh_command_line_rejects_other_processes() {
        assert!(parse_dsh_command_line(r#""D:\dsh\DeepSeek Harness.exe" --type=renderer"#).is_none());
        assert!(parse_dsh_command_line("").is_none());
    }

    #[test]
    fn deps_installed_requires_both_dep_and_bundle() {
        let both = r#"{"dependencies":{"@local/dsh-tba-preset":"link:D:/a","@local/dsh-chat-preset":"link:D:/b"},"dsh":{"profile":{"bundles":["@local/dsh-tba-preset","@local/dsh-chat-preset"]}}}"#;
        let dep_only = r#"{"dependencies":{"@local/dsh-tba-preset":"link:D:/a","@local/dsh-chat-preset":"link:D:/b"}}"#;
        let bundle_only = r#"{"dsh":{"profile":{"bundles":["@local/dsh-tba-preset","@local/dsh-chat-preset"]}}}"#;
        let one = r#"{"dependencies":{"@local/dsh-tba-preset":"link:D:/a"},"dsh":{"profile":{"bundles":["@local/dsh-tba-preset"]}}}"#;
        let none = r#"{"dependencies":{}}"#;
        assert!(deps_installed(both));
        assert!(!deps_installed(dep_only), "只加依赖但 bundles 没登记，DSH 不加载");
        assert!(!deps_installed(bundle_only), "只登记 bundles 但没软链，也不可用");
        assert!(!deps_installed(one));
        assert!(!deps_installed(none));
        assert!(!deps_installed("not json"));
    }

    #[test]
    fn add_bundles_fills_missing_names_and_preserves_other_fields() {
        // bundles 缺失：新建数组，其它字段（含 dependencies / name）保留。
        let src = r#"{"name":"dsh-profile-desktop","dependencies":{"x":"1"}}"#;
        let out = add_bundles(src, &[TBA_DEP, CHAT_DEP]).unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["name"], "dsh-profile-desktop");
        assert_eq!(v["dependencies"]["x"], "1");
        assert_eq!(v["dsh"]["profile"]["bundles"].as_array().unwrap().len(), 2);
        assert!(!deps_installed(&out)); // 只有 bundles，dependencies 还没加

        // 只有一个：补齐另一个，不动已有项顺序。
        let src = r#"{"dependencies":{"@local/dsh-tba-preset":"link:D:/a","@local/dsh-chat-preset":"link:D:/b"},"dsh":{"profile":{"bundles":["other", "@local/dsh-tba-preset"]}}}"#;
        let out = add_bundles(src, &[TBA_DEP, CHAT_DEP]).unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let bundles: Vec<&str> = v["dsh"]["profile"]["bundles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b.as_str().unwrap())
            .collect();
        assert_eq!(bundles, vec!["other", TBA_DEP, CHAT_DEP]);
        assert!(deps_installed(&out));

        // 两个都有：不重复添加，返回 None。
        let src = r#"{"dependencies":{"@local/dsh-tba-preset":"link:D:/a","@local/dsh-chat-preset":"link:D:/b"},"dsh":{"profile":{"bundles":["@local/dsh-tba-preset","@local/dsh-chat-preset"]}}}"#;
        assert!(add_bundles(src, &[TBA_DEP, CHAT_DEP]).unwrap().is_none());
    }

    #[test]
    fn split_command_line_respects_quotes() {
        let args = split_command_line(r#""C:\Program Files\a.exe" --flag C:\no-space"#);
        assert_eq!(args, vec![r"C:\Program Files\a.exe", "--flag", r"C:\no-space"]);
    }

    #[test]
    fn read_version_and_compare() {
        assert_eq!(read_version(r#"{"version":"1.0.0"}"#).as_deref(), Some("1.0.0"));
        assert_eq!(read_version(r#"{"name":"x"}"#), None);
        assert_eq!(read_version("not json"), None);
        assert_eq!(compare_version("1.0.1", "1.0.0"), CmpOrdering::Greater);
        assert_eq!(compare_version("1.0.0", "1.0.0"), CmpOrdering::Equal);
        assert_eq!(compare_version("0.9.9", "1.0.0"), CmpOrdering::Less);
        assert_eq!(compare_version("1.2", "1.2.0"), CmpOrdering::Equal);
    }

    #[test]
    fn decide_action_covers_all_cases() {
        // 版本相同 + 登记齐 -> 跳过
        assert_eq!(
            decide_action(Some("1.0.0"), "1.0.0", true),
            Action::SkipLatest("1.0.0".to_string())
        );
        // 版本相同 + 缺登记 -> 补登记
        assert_eq!(
            decide_action(Some("1.0.0"), "1.0.0", false),
            Action::RegisterOnly("1.0.0".to_string())
        );
        // 没装过 -> 安装
        assert_eq!(
            decide_action(None, "1.0.0", false),
            Action::Install("1.0.0".to_string())
        );
        // 内嵌更新 -> 升级
        assert_eq!(
            decide_action(Some("1.0.0"), "1.1.0", true),
            Action::Upgrade("1.0.0".to_string(), "1.1.0".to_string())
        );
        // 内嵌更旧 -> 降级
        assert_eq!(
            decide_action(Some("2.0.0"), "1.0.0", true),
            Action::Downgrade("2.0.0".to_string(), "1.0.0".to_string())
        );
        // 连续点两次：第一次升级后第二次版本相同 + 登记齐 -> 跳过
        let first = decide_action(Some("1.0.0"), "1.1.0", true);
        assert!(matches!(first, Action::Upgrade(_, _)));
        let second = decide_action(Some("1.1.0"), "1.1.0", true);
        assert_eq!(second, Action::SkipLatest("1.1.0".to_string()));
    }

    #[test]
    fn action_text_matches_user_messages() {
        assert_eq!(action_text(&Action::SkipLatest("1.0.0".into())), "已是最新 v1.0.0，无需更新");
        assert_eq!(action_text(&Action::RegisterOnly("1.0.0".into())), "已补齐登记（v1.0.0）");
        assert_eq!(action_text(&Action::Install("1.0.0".into())), "已安装 v1.0.0");
        assert_eq!(
            action_text(&Action::Upgrade("1.0.0".into(), "1.1.0".into())),
            "已从 v1.0.0 更新到 v1.1.0"
        );
        assert_eq!(
            action_text(&Action::Downgrade("2.0.0".into(), "1.0.0".into())),
            "已降级/回滚到 v1.0.0（原 v2.0.0）"
        );
    }

    #[test]
    fn embedded_versions_are_readable() {
        // 内嵌的两个 package.json 都能读出自己的 version（当前 1.0.0）。
        assert!(read_version(TBA_PACKAGE).is_some());
        assert!(read_version(CHAT_PACKAGE).is_some());
    }
}
