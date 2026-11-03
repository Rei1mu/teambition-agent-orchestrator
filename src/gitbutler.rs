//! GitButler / Git CLI 绑定：检测、状态、把 GitButler CLI 装到运行目录。
//!
//! **优先级高于 Teambition**：TBHub 每次启动先过这一关，缺 GitButler 就别往后走。
//!
//! Windows 上 GitButler 官方只发 MSI（桌面 app 安装包），没有独立 CLI 二进制；
//! 官方 `curl -fsSL https://gitbutler.com/install.sh | sh` 也只支持 mac/linux
//! （uname -s 在 Git Bash 下是 MINGW64_NT-*，脚本直接报错退出；
//! app.gitbutler.com/installers/info/windows/x86_64 也返回 404）。
//! 但 MSI 里带 `but.exe`（PFiles/GitButler/but.exe，实测自包含可直接运行），
//! 所以 Windows 自动安装走：
//!   下载 MSI zip -> Expand-Archive 解压 -> `msiexec /a /qn TARGETDIR=...` 管理抽取
//!   -> 拷贝 but.exe 到 <data>/bin/ -> `but --version` 验活。
//! 不需要管理员权限、不动注册表、删 data/bin 即卸载，也不影响用户自己装的桌面版。
//!
//! git 是系统级依赖（Git for Windows 装 Program Files 要提权），**不自动装，只检测+引导**。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::store::Store;

/// GitButler release 元数据地址（官方 installer 脚本也用这个）。
pub const RELEASES_URL: &str = "https://app.gitbutler.com/releases";
/// 官方一键安装命令（仅 mac/linux；Windows 只能走 MSI）。
pub const OFFICIAL_INSTALL_CMD: &str = "curl -fsSL https://gitbutler.com/install.sh | sh";
/// Git for Windows 官方下载页（git 缺失时引导用户自己装）。
pub const GIT_DOWNLOAD_URL: &str = "https://git-scm.com/download/win";
/// GitButler 官方下载页（手动安装用）。
pub const GB_DOWNLOAD_URL: &str = "https://gitbutler.com/downloads";

fn but_name() -> &'static str {
    if cfg!(windows) {
        "but.exe"
    } else {
        "but"
    }
}

/// 运行目录安装位置：`<data>/bin/but.exe`。data 跟 exe 走，已被 .gitignore 挡掉。
pub fn run_dir_bin(store: &Store) -> PathBuf {
    store.root.join("bin").join(but_name())
}

/// GitButler CLI 是从哪找到的。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GbSource {
    /// 环境变量显式指定（TBA_BUT / GITBUTLER_BIN / BUT_BIN）
    Env,
    /// 运行目录 data/bin（TBHub 自己装的）
    RunDir,
    /// PATH 里（用户自己装的）
    Path,
    /// 用户在窗口/命令行里手选的路径（存 config.toml 的 but_path）
    Config,
    /// GitButler 桌面版默认安装目录里带的 CLI
    DesktopApp,
}

impl GbSource {
    pub fn label(self) -> &'static str {
        match self {
            GbSource::Env => "环境变量",
            GbSource::Config => "手动指定",
            GbSource::RunDir => "运行目录",
            GbSource::Path => "PATH",
            GbSource::DesktopApp => "桌面安装",
        }
    }
}

/// GitButler CLI 检测结果。
#[derive(Clone, Debug)]
pub struct GbStatus {
    pub found: bool,
    pub path: Option<PathBuf>,
    pub source: Option<GbSource>,
    /// `but --version` 解析出来的版本号，如 0.22.3
    pub version: Option<String>,
    /// 当前目录是否已 `but setup` 成 GitButler 项目。None = 没装 / 查不到。
    pub project_setup: Option<bool>,
}

impl GbStatus {
    pub fn missing() -> Self {
        GbStatus {
            found: false,
            path: None,
            source: None,
            version: None,
            project_setup: None,
        }
    }
}

/// 当前平台的 release 元数据 key。空串 = 不支持的平台。
pub fn platform_key() -> &'static str {
    if cfg!(windows) {
        return "windows-x86_64";
    }
    match std::env::consts::ARCH {
        "aarch64" => "linux-aarch64",
        _ => "linux-x86_64",
    }
}

/// 从 release 元数据 JSON 里取本平台的下载地址（纯函数，可单测）。
pub fn pick_platform_url(json_text: &str, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json_text).ok()?;
    v.get("platforms")?
        .get(key)?
        .get("url")?
        .as_str()
        .map(|s| s.to_string())
}

/// `but --version` 输出形如 `but 0.22.3`，取版本号那段（纯函数，可单测）。
pub fn version_from_output(s: &str) -> Option<String> {
    s.split_whitespace()
        .find(|t| {
            t.chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        })
        .map(|t| t.trim().to_string())
}

/// `but status --json` 的输出 → 这个目录是不是 GitButler 项目（纯函数，可单测）。
///
/// **不能只看退出码**：目录没 setup 时 `but status --json` 退出码仍是 0，
/// 错误写在 stdout 的 JSON 里：`{"error":"setup_required","message":"No GitButler
/// project found at ."}`。早期只看退出码，把「没配置」误判成「已配置」。
/// 返回：明确是项目 = true；明确没配置 = false；其它（环境异常等）= None（宁可漏判）。
pub fn project_setup_from(ok: bool, stdout: &str, stderr: &str) -> Option<bool> {
    let all = format!("{stdout}
{stderr}");
    if all.contains("setup_required")
        || all.contains("Setup required")
        || all.contains("No GitButler project")
    {
        return Some(false);
    }
    if ok {
        return Some(true);
    }
    None
}

/// PATH 里按精确文件名找可执行文件。
fn path_lookup(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 可以指定 but 路径的环境变量。TBA_BUT 是本项目约定（跟 TBA_MSVC_BIN 一套），
/// 另外两个是社区常见写法，认一下不亏。值可以是 exe，也可以是目录。
pub const BUT_ENV_KEYS: &[&str] = &["TBA_BUT", "GITBUTLER_BIN", "BUT_BIN"];

/// 环境变量值 -> but 可执行文件。文件直接用；目录则拼 but 文件名；空/不存在 = None。
/// 纯函数，方便单测（不读真实环境）。
pub fn path_from_value(v: &str) -> Option<PathBuf> {
    let v = v.trim().trim_matches('"');
    if v.is_empty() {
        return None;
    }
    let p = PathBuf::from(v);
    if p.is_file() {
        return Some(p);
    }
    if p.is_dir() {
        let c = p.join(but_name());
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

/// 按 BUT_ENV_KEYS 的顺序读环境变量，第一个有效的算数。
fn env_but() -> Option<PathBuf> {
    for k in BUT_ENV_KEYS {
        let Ok(v) = std::env::var(k) else {
            continue;
        };
        if let Some(p) = path_from_value(&v) {
            return Some(p);
        }
    }
    None
}

/// GitButler 桌面版默认安装位置（MSI 装到 Program Files\GitButler，
/// 用户目录安装是 %LOCALAPPDATA%\Programs\GitButler）。
fn desktop_app_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |base: String, sub: &str| {
        if !base.is_empty() {
            out.push(PathBuf::from(base).join(sub));
        }
    };
    #[cfg(windows)]
    {
        push(
            std::env::var("LOCALAPPDATA").unwrap_or_default(),
            r"Programs\GitButler",
        );
        push(
            std::env::var("PROGRAMFILES").unwrap_or_default(),
            "GitButler",
        );
        push(
            std::env::var("PROGRAMFILES(X86)").unwrap_or_default(),
            "GitButler",
        );
        // 桌面版不同版本装过这些位置（用户级安装 / 漫游配置 / ProgramData）。
        push(std::env::var("LOCALAPPDATA").unwrap_or_default(), "GitButler");
        push(std::env::var("APPDATA").unwrap_or_default(), "GitButler");
        push(std::env::var("PROGRAMDATA").unwrap_or_default(), "GitButler");
    }
    out
}

fn run_version(path: &Path) -> Option<String> {
    let mut c = Command::new(path);
    c.arg("--version");
    crate::proc::hide(&mut c);
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    version_from_output(&String::from_utf8_lossy(&out.stdout))
}

fn run_project_setup(path: &Path) -> Option<bool> {
    let mut c = Command::new(path);
    c.args(["status", "--json"]);
    crate::proc::hide(&mut c);
    let out = c.output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    project_setup_from(out.status.success(), &stdout, &stderr)
}

/// 记住用户手选的 but 路径。
///
/// 会跑一次 `--version` 验活：选错文件（比如桌面版主程序 `GitButler.exe`）
/// 要当场说清楚，而不是存进去、下次照样报「没装」。存 `config.toml` 的 but_path，
/// 下次 detect 直接命中，来源显示「手动指定」。
pub fn set_path(store: &Store, raw: &str) -> Result<String, String> {
    let p = path_from_value(raw).ok_or_else(|| {
        format!(
            "路径不存在，或者目录里没有 {}：{raw}",
            but_name()
        )
    })?;
    let ver = run_version(&p).ok_or_else(|| {
        format!(
            "这个文件跑不起来（不是 but CLI？）：{}\n桌面版里的 CLI 文件名就叫 {}，一般在安装目录下。",
            p.display(),
            but_name()
        )
    })?;
    let mut cfg = store.load_config();
    cfg.but_path = p.display().to_string();
    store.save_config(&cfg)?;
    Ok(format!("已记住 GitButler CLI：{}（but {ver}）", p.display()))
}

/// 清掉手选路径，回到自动检测（环境变量 / 运行目录 / PATH / 已知安装目录）。
pub fn clear_path(store: &Store) -> Result<String, String> {
    let mut cfg = store.load_config();
    if cfg.but_path.is_empty() {
        return Ok("没有手选过路径（本来就是自动检测）".to_string());
    }
    cfg.but_path.clear();
    store.save_config(&cfg)?;
    Ok("已清除手选路径，回到自动检测".to_string())
}

/// 检测 GitButler CLI。顺序（前面的赢）：
/// **环境变量 > 运行目录 > PATH > 已知安装目录 > 扫常见根目录**。
///
/// 最后一步是兜底：桌面版不同版本装的位置不一样（Program Files / LOCALAPPDATA /
/// ProgramData 都见过），固定路径猜不全，就按目录名找。这一步要遍历目录，所以
/// 只有前面全落空才做，正常机器不会付出这个代价。
///
/// 纯本地操作（最多跑两个子进程取版本/工作区状态），不联网。
pub fn detect(store: &Store) -> GbStatus {
    let mut quick: Vec<(PathBuf, GbSource)> = Vec::new();
    if let Some(p) = env_but() {
        quick.push((p, GbSource::Env));
    }
    // 用户手选过的路径优先于运行目录：他指定了就用他指定的。
    let cfg = store.load_config();
    if !cfg.but_path.trim().is_empty() {
        if let Some(p) = path_from_value(&cfg.but_path) {
            quick.push((p, GbSource::Config));
        }
    }
    quick.push((run_dir_bin(store), GbSource::RunDir));
    for name in [but_name(), "git-butler", "git-butler.exe"] {
        if let Some(p) = path_lookup(name) {
            quick.push((p, GbSource::Path));
        }
    }
    for d in desktop_app_dirs() {
        quick.push((d.join(but_name()), GbSource::DesktopApp));
    }
    // 不扫盘：找不到就让用户指一次（窗口里有「选择 but.exe」按钮，
    // 命令行用 `tba gitbutler set --path`）。扫盘又慢又可能挑到错误的副本。
    probe(&quick).unwrap_or_else(GbStatus::missing)
}

/// 按顺序试候选路径，第一个存在的就取版本/工作区状态。
fn probe(cands: &[(PathBuf, GbSource)]) -> Option<GbStatus> {
    for (p, src) in cands {
        if !p.is_file() {
            continue;
        }
        return Some(GbStatus {
            found: true,
            path: Some(p.clone()),
            source: Some(*src),
            version: run_version(p),
            project_setup: run_project_setup(p),
        });
    }
    None
}

/// git 在不在（GitButler CLI 依赖 git）。返回版本号，如 2.47.1。
pub fn git_available() -> Option<String> {
    let mut c = Command::new("git");
    c.arg("--version");
    crate::proc::hide(&mut c);
    let out = c.output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .last()
        .map(|v| v.trim().to_string())
}

/// 递归找目录下第一个指定文件名的文件。
fn find_by_name(dir: &Path, name: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_by_name(&p, name) {
                return Some(f);
            }
        } else if p.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

/// 递归找目录下第一个指定扩展名的文件。
fn find_by_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_by_ext(&p, ext) {
                return Some(f);
            }
        } else if p.extension().map(|x| x == ext).unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

fn fetch_text(url: &str) -> Result<String, String> {
    let r = reqwest::blocking::get(url).map_err(|e| format!("请求 {url} 失败: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("请求 {url} 返回 HTTP {}", r.status()));
    }
    r.text().map_err(|e| format!("读响应失败: {e}"))
}

fn fetch_binary(url: &str, dest: &Path) -> Result<(), String> {
    let mut r = reqwest::blocking::get(url).map_err(|e| format!("下载 {url} 失败: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("下载 {url} 返回 HTTP {}", r.status()));
    }
    let mut f = std::fs::File::create(dest).map_err(|e| format!("创建文件失败: {e}"))?;
    std::io::copy(&mut r, &mut f).map_err(|e| format!("写文件失败: {e}"))?;
    Ok(())
}

fn run_ps(script: &str) -> Result<(), String> {
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    crate::proc::hide(&mut c);
    let status = c
        .status()
        .map_err(|e| format!("运行 PowerShell 失败: {e}"))?;
    if !status.success() {
        return Err("PowerShell 执行失败".to_string());
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)
        .map_err(|e| format!("读文件属性失败: {e}"))?
        .permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(p, perm).map_err(|e| format!("设可执行权限失败: {e}"))
}

// Windows 不需要可执行位；留着同名空桩让 linux 分支在 Windows 上也能编译。
#[cfg(not(unix))]
fn set_executable(_p: &Path) -> Result<(), String> {
    Ok(())
}

/// 把 GitButler CLI 装到运行目录 `<data>/bin/`，返回装好的路径。
///
/// - Windows：下载 MSI zip → 解压 → `msiexec /a` 管理抽取 → 拷贝 but.exe → 验活。
/// - Linux：release 元数据直接给 but 二进制，下载 + chmod。
/// - macOS：官方只发 .app 包，不自动装，返回引导信息。
pub fn install(store: &Store) -> Result<PathBuf, String> {
    if cfg!(target_os = "macos") {
        return Err(format!(
            "macOS 不自动安装（官方只发 .app 包）。请手动执行: {OFFICIAL_INSTALL_CMD}"
        ));
    }
    let key = platform_key();
    if key.is_empty() {
        return Err(format!("不支持的平台。请手动执行: {OFFICIAL_INSTALL_CMD}"));
    }
    let meta = fetch_text(RELEASES_URL)?;
    let url = pick_platform_url(&meta, key)
        .ok_or_else(|| format!("release 元数据里没有平台 {key} 的下载地址"))?;

    let work = store.root.join("bin").join(".gb-install");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("创建安装目录失败: {e}"))?;

    let dest = run_dir_bin(store);
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| format!("创建 bin 目录失败: {e}"))?;

    if cfg!(windows) {
        // 1. 下载 MSI zip（约 36MB）
        let zip = work.join("gitbutler.msi.zip");
        fetch_binary(&url, &zip)?;
        // 2. 解 zip：用 Windows 自带的 PowerShell Expand-Archive（GNU tar 不认 zip）
        let ps = format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            zip.display(),
            work.display()
        );
        run_ps(&ps)?;
        // 3. msiexec /a = 管理安装：只解出文件，不注册、不需要管理员
        let msi = find_by_ext(&work, "msi").ok_or("zip 里没找到 .msi")?;
        let target = work.join("x");
        let mut cmd = Command::new("msiexec");
        crate::proc::hide(&mut cmd);
        let status = cmd
            .args([
                "/a",
                &msi.to_string_lossy(),
                "/qn",
                &format!("TARGETDIR={}", target.display()),
            ])
            .status()
            .map_err(|e| format!("运行 msiexec 失败: {e}"))?;
        if !status.success() {
            return Err(format!(
                "msiexec 抽取失败（退出码 {:?}）。可手动安装: {GB_DOWNLOAD_URL}",
                status.code()
            ));
        }
        // 4. 拿出 but.exe 拷到运行目录
        let exe = find_by_name(&target, but_name())
            .ok_or("MSI 里没找到 but.exe（PFiles/GitButler/but.exe）")?;
        std::fs::copy(&exe, &dest).map_err(|e| format!("拷贝 but.exe 失败: {e}"))?;
    } else {
        // Linux：元数据 URL 直接就是 but 二进制
        fetch_binary(&url, &dest)?;
        set_executable(&dest)?;
    }

    // 5. 验活：跑不起来就等于没装
    let v = run_version(&dest).ok_or_else(|| {
        format!(
            "安装后校验失败: but --version 跑不起来。可手动安装: {GB_DOWNLOAD_URL}"
        )
    })?;
    let _ = std::fs::remove_dir_all(&work);
    println!("GitButler CLI {v} 已装到 {}", dest.display());
    Ok(dest)
}

/// 缺 GitButler / 缺 git 时打印给用户的引导（CLI 用）。
pub fn print_missing_guidance(gb: &GbStatus, git_ver: &Option<String>) {
    if !gb.found {
        eprintln!("✗ 未检测到 GitButler CLI（but）。");
        eprintln!("  GitButler 优先级高于 Teambition——agent 靠它管分支/提交，先装它。");
        eprintln!("  自动安装（装到运行目录 data/bin，无需管理员、不动注册表）:");
        eprintln!("      tb gitbutler install");
        eprintln!("  Windows 官方只发 MSI，自动安装走「下载 MSI -> 抽取 but.exe」，上面的命令即可。");
        eprintln!("  已经找过: 环境变量 TBA_BUT/GITBUTLER_BIN/BUT_BIN、data/bin、PATH、常见安装目录。");
        eprintln!("  but 装在别处就指定一次（记到 config.toml，下次直接用）:");
        eprintln!("      tb gitbutler set --path \"D:\\tools\\but.exe\"");
        eprintln!("  窗口里也有「选择 but.exe」按钮；临时覆盖可用环境变量 TBA_BUT。");
        eprintln!("  也可以自己装: {GB_DOWNLOAD_URL}");
        eprintln!("  官方一键安装（仅 mac/linux）: {OFFICIAL_INSTALL_CMD}");
    }
    if git_ver.is_none() {
        eprintln!("✗ 未检测到 git（GitButler CLI 依赖 git）。");
        eprintln!("  git 是系统级依赖，不自动安装，请先装 Git for Windows: {GIT_DOWNLOAD_URL}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("tba-gb-test-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        Store { root }
    }

    #[test]
    fn pick_platform_url_reads_windows_asset() {
        let json = r#"{"version":"0.22.3","platforms":{
            "windows-x86_64":{"url":"https://releases.gitbutler.com/x.msi.zip"},
            "linux-x86_64":{"url":"https://releases.gitbutler.com/but"}}}"#;
        assert_eq!(
            pick_platform_url(json, "windows-x86_64").as_deref(),
            Some("https://releases.gitbutler.com/x.msi.zip")
        );
        assert_eq!(pick_platform_url(json, "darwin-x86_64"), None);
        assert_eq!(pick_platform_url("not json", "windows-x86_64"), None);
    }

    #[test]
    fn version_from_output_parses_but_version() {
        assert_eq!(version_from_output("but 0.22.3").as_deref(), Some("0.22.3"));
        assert_eq!(version_from_output("but 0.22.3\n").as_deref(), Some("0.22.3"));
        assert_eq!(version_from_output("no version here"), None);
    }

    #[test]
    fn project_setup_from_maps_exit_and_output() {
        assert_eq!(project_setup_from(true, "", ""), Some(true));
        assert_eq!(
            project_setup_from(false, "", "Error: Setup required: No GitButler project found at ."),
            Some(false)
        );
        assert_eq!(project_setup_from(false, "", "some other failure"), None);
        // 真实情况：没 setup 时退出码是 0，错误只在 stdout 的 JSON 里。
        // 只看退出码会把「没配置」误判成「已配置」（踩过）。
        assert_eq!(
            project_setup_from(
                true,
                r#"{"error":"setup_required","message":"No GitButler project found at ."}"#,
                ""
            ),
            Some(false)
        );
    }

    #[test]
    fn run_dir_bin_points_into_data_bin() {
        let s = tmp_store("bindir");
        let p = run_dir_bin(&s);
        assert!(p.ends_with(if cfg!(windows) { "but.exe" } else { "but" }));
        assert!(p.parent().unwrap().ends_with("bin"));
    }

    #[test]
    fn detect_finds_run_dir_install_first() {
        let s = tmp_store("detect");
        let bin = run_dir_bin(&s);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"fake").unwrap();
        let st = detect(&s);
        assert!(st.found, "运行目录有 but 就算找到");
        assert_eq!(st.source, Some(GbSource::RunDir));
        assert_eq!(st.path.as_ref(), Some(&bin));
    }

    // 环境变量的值可以是 exe，也可以是目录；空/带引号/不存在都要处理对。
    #[test]
    fn path_from_value_accepts_file_and_dir() {
        let s = tmp_store("envpath");
        let bin = s.root.join("x").join(but_name());
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"fake").unwrap();

        assert_eq!(path_from_value(&bin.display().to_string()), Some(bin.clone()));
        // 给目录也行：自动拼 but 文件名
        assert_eq!(
            path_from_value(&bin.parent().unwrap().display().to_string()),
            Some(bin.clone())
        );
        // 环境变量里常带引号（Windows 路径有空格时）
        assert_eq!(
            path_from_value(&format!("\"{}\"", bin.display())),
            Some(bin.clone())
        );
        assert_eq!(path_from_value("   "), None);
        assert_eq!(path_from_value(r"D:\definitely\not\here.exe"), None);
    }

    // 手选路径要验活：不存在的、不是可执行文件的都要当场拒绝，且不能写进配置。
    #[test]
    fn set_path_rejects_bad_input() {
        let s = tmp_store("setbad");
        assert!(set_path(&s, r"D:\definitely
ot\here.exe").is_err());

        let fake = s.root.join("fake-but.exe");
        std::fs::write(&fake, b"not a pe file").unwrap();
        let e = set_path(&s, &fake.display().to_string()).unwrap_err();
        assert!(e.contains("跑不起来"), "应提示不是有效程序: {e}");
        assert!(
            s.load_config().but_path.is_empty(),
            "验活失败不能写进配置"
        );

        // 目录里没有 but.exe 也要拒
        let empty = s.root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(set_path(&s, &empty.display().to_string()).is_err());
    }

    // 配置里的路径要压过运行目录/PATH（用户手选的优先），detect 来源显示「手动指定」。
    #[test]
    fn detect_uses_configured_path() {
        let s = tmp_store("cfgdetect");
        let fake = s.root.join("fake-but.exe");
        std::fs::write(&fake, b"fake").unwrap();
        let mut cfg = s.load_config();
        cfg.but_path = fake.display().to_string();
        s.save_config(&cfg).unwrap();

        let st = detect(&s);
        assert!(st.found);
        assert_eq!(st.source, Some(GbSource::Config));
        assert_eq!(st.path, Some(fake));

        // 清掉后就不该再认它
        clear_path(&s).unwrap();
        assert!(s.load_config().but_path.is_empty());
    }

    #[test]
    fn platform_key_is_nonempty_on_supported_platforms() {
        assert!(!platform_key().is_empty());
    }
}
