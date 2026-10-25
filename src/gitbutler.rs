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
    /// 运行目录 data/bin（TBHub 自己装的）
    RunDir,
    /// PATH 里（用户自己装的）
    Path,
    /// GitButler 桌面版默认安装目录里带的 CLI
    DesktopApp,
}

impl GbSource {
    pub fn label(self) -> &'static str {
        match self {
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

/// `but status --json` 的退出情况 → 当前目录是不是 GitButler 项目（纯函数，可单测）。
/// 成功 = true；报 Setup required = false；其它（环境异常等）= None（宁可漏判不误判）。
pub fn project_setup_from(ok: bool, stderr: &str) -> Option<bool> {
    if ok {
        return Some(true);
    }
    if stderr.contains("Setup required") || stderr.contains("No GitButler project") {
        return Some(false);
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
    }
    out
}

fn run_version(path: &Path) -> Option<String> {
    let out = Command::new(path).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    version_from_output(&String::from_utf8_lossy(&out.stdout))
}

fn run_project_setup(path: &Path) -> Option<bool> {
    let out = Command::new(path).args(["status", "--json"]).output().ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    project_setup_from(out.status.success(), &stderr)
}

/// 检测 GitButler CLI。顺序：**运行目录 > PATH > 桌面安装目录**。
/// 纯本地操作（最多跑两个子进程取版本/工作区状态），不联网。
pub fn detect(store: &Store) -> GbStatus {
    let mut candidates: Vec<(PathBuf, GbSource)> = vec![(run_dir_bin(store), GbSource::RunDir)];
    for name in [but_name(), "git-butler", "git-butler.exe"] {
        if let Some(p) = path_lookup(name) {
            candidates.push((p, GbSource::Path));
        }
    }
    for d in desktop_app_dirs() {
        candidates.push((d.join(but_name()), GbSource::DesktopApp));
    }
    for (p, src) in candidates {
        if !p.is_file() {
            continue;
        }
        let version = run_version(&p);
        let project_setup = run_project_setup(&p);
        return GbStatus {
            found: true,
            path: Some(p),
            source: Some(src),
            version,
            project_setup,
        };
    }
    GbStatus::missing()
}

/// git 在不在（GitButler CLI 依赖 git）。返回版本号，如 2.47.1。
pub fn git_available() -> Option<String> {
    let out = Command::new("git").arg("--version").output().ok()?;
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
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
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
        let status = Command::new("msiexec")
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
        let root = std::env::temp_dir().join(format!("tbhub-gb-test-{tag}"));
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
    fn project_setup_from_maps_exit_and_stderr() {
        assert_eq!(project_setup_from(true, ""), Some(true));
        assert_eq!(
            project_setup_from(false, "Error: Setup required: No GitButler project found at ."),
            Some(false)
        );
        assert_eq!(project_setup_from(false, "some other failure"), None);
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

    #[test]
    fn platform_key_is_nonempty_on_supported_platforms() {
        assert!(!platform_key().is_empty());
    }
}
