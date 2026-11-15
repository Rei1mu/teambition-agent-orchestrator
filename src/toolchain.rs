// 编译工具链：MSVC 的 link.exe + Windows SDK 的库目录。
//
// 这是编译 Windows 软件 / DLL 要用到的工具链（不只是编 tba 本体）。
// 三条来源，优先级从高到低：
//   1. 环境变量 TBA_MSVC_BIN / TBA_MSVC_LIB / TBA_SDK_LIB（CI、临时覆盖）
//   2. config.toml 的 msvc_bin / msvc_lib / winsdk_libs（手选一次，之后一直用）
//   3. 自动探测（vswhere 找 VS + Windows Kits 标准目录）
//
// 探测不到不扫盘：装了 VS 预览版 / 多个 SDK 版本时，扫盘容易挑错那一份，
// 让用户点一次「指定位置」比猜准，也比等十几秒扫盘强。
// 真没装就给安装地址（doctor / 设置页都会显示）。

use crate::proc;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Toolchain {
    #[serde(default)]
    pub msvc_bin: String,
    #[serde(default)]
    pub msvc_lib: String,
    // Windows SDK 的 lib 目录，可多个用 ; 分隔（um\\x64;ucrt\\x64）
    #[serde(default)]
    pub winsdk_libs: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    Config,
    Auto,
    Missing,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Env => "环境变量",
            Source::Config => "手动指定",
            Source::Auto => "自动探测",
            Source::Missing => "没找到",
        }
    }
}

#[derive(Clone)]
pub struct Resolved {
    pub tc: Toolchain,
    pub source: Source,
    pub problems: Vec<String>,
}

impl Resolved {
    pub fn ready(&self) -> bool {
        self.problems.is_empty()
    }
    pub fn needs_install(&self) -> bool {
        matches!(self.source, Source::Missing)
    }
}

// 没装时给的安装地址。
pub const MSVC_URL: &str = "https://visualstudio.microsoft.com/zh-hans/downloads/";
pub const WINSODK_URL: &str =
    "https://developer.microsoft.com/windows/downloads/windows-sdk/";

pub fn path(store: &Store) -> PathBuf {
    store.root.join("toolchain.toml")
}

pub fn load(store: &Store) -> Toolchain {
    std::fs::read_to_string(path(store))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(store: &Store, tc: &Toolchain) -> Result<(), String> {
    let p = path(store);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {e}"))?;
    }
    let s = toml::to_string_pretty(tc).map_err(|e| format!("序列化失败: {e}"))?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, s).map_err(|e| format!("写 {} 失败: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("替换 {} 失败: {e}", p.display()))?;
    Ok(())
}

fn has_file(dir: &str, name: &str) -> bool {
    let d = dir.trim();
    !d.is_empty() && Path::new(d).join(name).is_file()
}

fn has_file_any(dirs: &str, name: &str) -> bool {
    dirs.split(';').any(|d| has_file(d, name))
}

// 验活：三个目录各挑一个「必须存在」的文件当探针。
// 空字符串也算问题——「没配」和「配了但不对」要分开说。
pub fn check(tc: &Toolchain) -> Vec<String> {
    let mut p = Vec::new();
    if tc.msvc_bin.trim().is_empty() {
        p.push("缺 MSVC 的 bin 目录（link.exe 所在）".to_string());
    } else if !has_file(&tc.msvc_bin, "link.exe") {
        p.push(format!("{} 里没有 link.exe", tc.msvc_bin.trim()));
    }
    if tc.msvc_lib.trim().is_empty() {
        p.push("缺 MSVC 的 lib 目录（libcmt.lib 所在）".to_string());
    } else if !has_file(&tc.msvc_lib, "libcmt.lib") {
        p.push(format!("{} 里没有 libcmt.lib", tc.msvc_lib.trim()));
    }
    if tc.winsdk_libs.trim().is_empty() {
        p.push("缺 Windows SDK 的 lib 目录（kernel32.lib 所在）".to_string());
    } else if !has_file_any(&tc.winsdk_libs, "kernel32.lib") {
        p.push(format!("{} 里没有 kernel32.lib", tc.winsdk_libs.trim()));
    }
    p
}

// 三个探针是否齐，给 doctor / 设置页打勾用。
pub fn verify(tc: &Toolchain) -> (bool, bool, bool) {
    let has = |dir: &str, name: &str| has_file(dir, name);
    (
        has(&tc.msvc_bin, "link.exe"),
        has(&tc.msvc_lib, "libcmt.lib"),
        tc.winsdk_libs.split(';').any(|d| has(d, "kernel32.lib")),
    )
}

fn env_or(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

fn vswhere() -> Option<PathBuf> {
    // 不依赖环境变量（DSH / 子进程里 ProgramFiles(x86) 常取不到），直接试常见安装位。
    let candidates = [
        "C:\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe",
        "C:\\Program Files\\Microsoft Visual Studio\\Installer\\vswhere.exe",
        "D:\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe",
        "D:\\Program Files\\Microsoft Visual Studio\\Installer\\vswhere.exe",
    ];
    for c in candidates {
        let p = Path::new(c);
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }
    // 环境变量兜底
    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Ok(pf) = std::env::var(key) {
            let p = Path::new(&pf)
                .join("Microsoft Visual Studio")
                .join("Installer")
                .join("vswhere.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn vs_install_path() -> Option<String> {
    let vsw = vswhere()?;
    let mut cmd = Command::new(vsw);
    cmd.args(["-latest", "-products", "*", "-property", "installationPath"]);
    proc::hide(&mut cmd);
    let out = cmd.output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn ver_key(name: &str) -> Vec<u32> {
    name.split('.').map(|x| x.parse().unwrap_or(0)).collect()
}

fn detect_msvc() -> Option<(String, String)> {
    let vs = vs_install_path()?;
    let root = Path::new(&vs).join("VC").join("Tools").join("MSVC");
    let mut dirs: Vec<String> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    dirs.sort_by_key(|d| ver_key(d));
    for v in dirs.iter().rev() {
        let bin = root.join(v).join("bin").join("Hostx64").join("x64");
        let lib = root.join(v).join("lib").join("x64");
        if bin.join("link.exe").is_file() {
            return Some((bin.display().to_string(), lib.display().to_string()));
        }
    }
    None
}

fn detect_sdk() -> Option<String> {
    // 不依赖环境变量，直接试常见安装位（含空格的 Program Files (x86)）。
    let candidates = [
        "C:\\Program Files (x86)\\Windows Kits\\10\\Lib",
        "C:\\Program Files\\Windows Kits\\10\\Lib",
        "D:\\Program Files (x86)\\Windows Kits\\10\\Lib",
        "D:\\Program Files\\Windows Kits\\10\\Lib",
    ];
    let libroot = candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir())
        .or_else(|| {
            for key in ["ProgramFiles(x86)", "ProgramFiles"] {
                if let Ok(pf) = std::env::var(key) {
                    let p = Path::new(&pf).join("Windows Kits").join("10").join("Lib");
                    if p.is_dir() {
                        return Some(p);
                    }
                }
            }
            None
        })?;
    let mut vers: Vec<String> = std::fs::read_dir(&libroot)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    vers.sort_by_key(|d| ver_key(d));
    for v in vers.iter().rev() {
        let base = libroot.join(v);
        let um = base.join("um").join("x64");
        if !um.join("kernel32.lib").is_file() {
            continue;
        }
        let ucrt = base.join("ucrt").join("x64");
        return Some(if ucrt.is_dir() {
            format!("{};{}", um.display(), ucrt.display())
        } else {
            um.display().to_string()
        });
    }
    None
}

pub fn auto_detect() -> Option<Toolchain> {
    let (bin, lib) = detect_msvc()?;
    let sdk = detect_sdk().unwrap_or_default();
    Some(Toolchain {
        msvc_bin: bin,
        msvc_lib: lib,
        winsdk_libs: sdk,
    })
}

/// 解析当前该用哪份路径。返回的 problems 非空 = 用不了，调用方要把它们原样告诉用户。
pub fn resolve(store: &Store) -> Resolved {
    let env_tc = Toolchain {
        msvc_bin: env_or("TBA_MSVC_BIN"),
        msvc_lib: env_or("TBA_MSVC_LIB"),
        winsdk_libs: env_or("TBA_SDK_LIB"),
    };
    if !env_tc.msvc_bin.is_empty() || !env_tc.msvc_lib.is_empty() || !env_tc.winsdk_libs.is_empty() {
        let problems = check(&env_tc);
        return Resolved { tc: env_tc, source: Source::Env, problems };
    }
    let cfg = load(store);
    if !cfg.msvc_bin.is_empty() || !cfg.msvc_lib.is_empty() || !cfg.winsdk_libs.is_empty() {
        let problems = check(&cfg);
        return Resolved { tc: cfg, source: Source::Config, problems };
    }
    if let Some(auto) = auto_detect() {
        let problems = check(&auto);
        return Resolved { tc: auto, source: Source::Auto, problems };
    }
    Resolved {
        tc: Toolchain::default(),
        source: Source::Missing,
        problems: vec![
            "没找到 MSVC / Windows SDK（标准位置没有；用 tba 设置页指定一次，或去安装）".to_string(),
        ],
    }
}
