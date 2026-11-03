// 构建工具链：MSVC 的 link.exe + Windows SDK 的库目录。
//
// 为什么只认 MSVC：发布包就是 msvc 目标编出来的。GNU 工具链的 exe 依赖 mingw
// 运行时，窗口、路径、依赖行为都不一样，所以**不做 gnu 兜底**——找不到就明确
// 报错并让用户指定一次位置，而不是悄悄换一套工具链编出不一样的东西。
//
// 三条来源，优先级从高到低：
//   1. 环境变量 TBA_MSVC_BIN / TBA_MSVC_LIB / TBA_SDK_LIB / TBA_EXTRA_LIB（CI、临时覆盖）
//   2. data/toolchain.toml（手选一次，之后一直用）
//   3. 自动探测（vswhere 找 VS + Windows Kits 标准目录）
//
// 探测不到**不扫盘**：装了 VS 预览版 / 多个 SDK 版本时，扫盘很容易挑错那一份，
// 让用户点一次「指定位置」比猜准，也比等几十秒扫盘强。
//
// 路径字段的语义（跟 assets/reload.ps1 的 PATH/LIB 拼法一一对应）：
//   msvc_bin → PATH 前置，link.exe 在这
//   msvc_lib → LIB 第一段，libcmt.lib 等 CRT
//   sdk_lib  → LIB 第二段，kernel32.lib 等系统库（可多个，; 分隔）
//   extra_lib→ LIB 最后一段，用户自己补的库

use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const TC_FILE: &str = "toolchain.toml";

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Toolchain {
    #[serde(default)]
    pub msvc_bin: String,
    #[serde(default)]
    pub msvc_lib: String,
    #[serde(default)]
    pub sdk_lib: String,
    #[serde(default)]
    pub extra_lib: String,
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

/// 一次解析的结果：用哪份路径、从哪来的、还差什么。
pub struct Resolved {
    pub tc: Toolchain,
    pub source: Source,
    pub problems: Vec<String>,
}

impl Resolved {
    pub fn ready(&self) -> bool {
        self.problems.is_empty()
    }
}

pub fn path(store: &Store) -> PathBuf {
    store.root.join(TC_FILE)
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
    // 先写临时文件再改名：写一半崩了不会留下半截 toolchain.toml。
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

/// 验活：三个目录各挑一个「必须存在」的文件当探针。
/// 空字符串也算问题——「没配」和「配了但不对」要分开说，否则用户不知道该点哪里。
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
    if tc.sdk_lib.trim().is_empty() {
        p.push("缺 Windows SDK 的 lib 目录（kernel32.lib 所在）".to_string());
    } else if !has_file_any(&tc.sdk_lib, "kernel32.lib") {
        p.push(format!("{} 里没有 kernel32.lib", tc.sdk_lib.trim()));
    }
    p
}

fn env_or(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

fn vswhere() -> Option<PathBuf> {
    let pf = std::env::var("ProgramFiles(x86)")
        .or_else(|_| std::env::var("ProgramFiles"))
        .ok()?;
    let p = Path::new(&pf)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    p.is_file().then_some(p)
}

// vswhere 是 VS 官方给的方式：比猜安装目录稳，也不怕装了多个版本。
fn vs_install_path() -> Option<String> {
    let vsw = vswhere()?;
    let mut cmd = Command::new(vsw);
    cmd.args(["-latest", "-products", "*", "-property", "installationPath"]);
    crate::proc::hide(&mut cmd);
    let out = cmd.output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

// 目录名是 14.44.35207 这种，按数字段比，别按字符串比（"14.9" > "14.44" 是错的）。
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
    // 从高版本往下试，挑第一个真的有 link.exe 的（有的版本只装了 IDE 没装工具集）。
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
    let pf = std::env::var("ProgramFiles(x86)")
        .or_else(|_| std::env::var("ProgramFiles"))
        .ok()?;
    let libroot = Path::new(&pf).join("Windows Kits").join("10").join("Lib");
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
        // ucrt 有就一起给（msvcrt 系列在那边），没有就只给 um。
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
        sdk_lib: sdk,
        extra_lib: String::new(),
    })
}

/// 解析当前该用哪份路径。返回的 problems 非空 = 用不了，调用方要把它们原样告诉用户。
pub fn resolve(store: &Store) -> Resolved {
    let env_tc = Toolchain {
        msvc_bin: env_or("TBA_MSVC_BIN"),
        msvc_lib: env_or("TBA_MSVC_LIB"),
        sdk_lib: env_or("TBA_SDK_LIB"),
        extra_lib: env_or("TBA_EXTRA_LIB"),
    };
    if !env_tc.msvc_bin.is_empty() || !env_tc.msvc_lib.is_empty() || !env_tc.sdk_lib.is_empty() {
        let problems = check(&env_tc);
        return Resolved {
            tc: env_tc,
            source: Source::Env,
            problems,
        };
    }
    let cfg = load(store);
    if !cfg.msvc_bin.is_empty() || !cfg.msvc_lib.is_empty() || !cfg.sdk_lib.is_empty() {
        let problems = check(&cfg);
        return Resolved {
            tc: cfg,
            source: Source::Config,
            problems,
        };
    }
    if let Some(auto) = auto_detect() {
        let problems = check(&auto);
        return Resolved {
            tc: auto,
            source: Source::Auto,
            problems,
        };
    }
    Resolved {
        tc: Toolchain::default(),
        source: Source::Missing,
        problems: vec![
            "没找到 MSVC / Windows SDK（标准位置没有；用 tba toolchain set 指定一次）".to_string(),
        ],
    }
}

/// 手选：只改传进来的字段，其余保留。改完立刻验活，路径不对当场报。
pub fn set(
    store: &Store,
    bin: Option<&str>,
    lib: Option<&str>,
    sdk: Option<&str>,
    extra: Option<&str>,
) -> Result<String, String> {
    let mut tc = load(store);
    // 之前是自动探测出来的（配置还空着）：以探测结果为底，只覆盖用户改的那几项。
    // 否则用户只指定 SDK 位置时，MSVC 那两段会被清空。
    if tc.msvc_bin.is_empty() && tc.msvc_lib.is_empty() {
        if let Some(auto) = auto_detect() {
            tc = auto;
        }
    }
    if let Some(v) = bin {
        tc.msvc_bin = v.trim().to_string();
    }
    if let Some(v) = lib {
        tc.msvc_lib = v.trim().to_string();
    }
    if let Some(v) = sdk {
        tc.sdk_lib = v.trim().to_string();
    }
    if let Some(v) = extra {
        tc.extra_lib = v.trim().to_string();
    }
    let problems = check(&tc);
    if !problems.is_empty() {
        return Err(format!(
            "这份路径还不能用（没有保存）：\n  - {}",
            problems.join("\n  - ")
        ));
    }
    save(store, &tc)?;
    Ok(format!("已保存构建工具链：\n{}", show(&tc)))
}

pub fn clear(store: &Store) -> Result<String, String> {
    let p = path(store);
    if p.exists() {
        std::fs::remove_file(&p).map_err(|e| format!("删除 {} 失败: {e}", p.display()))?;
    }
    Ok("已清空手动指定的路径，下次改用自动探测。".to_string())
}

/// 自动探测一遍并写盘。写盘后就是「手动指定」来源，不会再被探测结果悄悄覆盖。
pub fn guess(store: &Store) -> Result<String, String> {
    let auto = auto_detect().ok_or_else(|| {
        "标准位置没找到 MSVC / Windows SDK。装一下 VS 的「使用 C++ 的桌面开发」\
         （带 Windows SDK），或者用 tba toolchain set 指定位置。"
            .to_string()
    })?;
    let problems = check(&auto);
    if !problems.is_empty() {
        return Err(format!(
            "探测到的路径不完整：\n  - {}",
            problems.join("\n  - ")
        ));
    }
    save(store, &auto)?;
    Ok(format!("已探测并保存：\n{}", show(&auto)))
}

pub fn show(tc: &Toolchain) -> String {
    let dash = |s: &str| {
        if s.trim().is_empty() {
            "（空）".to_string()
        } else {
            s.trim().to_string()
        }
    };
    format!(
        "  MSVC bin : {}\n  MSVC lib : {}\n  SDK lib  : {}\n  额外 lib : {}",
        dash(&tc.msvc_bin),
        dash(&tc.msvc_lib),
        dash(&tc.sdk_lib),
        dash(&tc.extra_lib)
    )
}

/// 给子进程（reload.ps1 里的 cargo）注入 PATH / LIB。
/// 只注入非空的段：空串进 PATH 会变成当前目录，进 LIB 会让链接器去扫一遍 cwd。
pub fn apply_env(cmd: &mut Command, tc: &Toolchain) {
    let bin = tc.msvc_bin.trim();
    if !bin.is_empty() {
        let old = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{bin};{old}"));
        cmd.env("TBA_MSVC_BIN", bin);
    }
    let mut libs: Vec<&str> = Vec::new();
    for d in [&tc.msvc_lib, &tc.sdk_lib, &tc.extra_lib] {
        for one in d.split(';').map(str::trim).filter(|x| !x.is_empty()) {
            libs.push(one);
        }
    }
    if !libs.is_empty() {
        cmd.env("LIB", libs.join(";"));
        cmd.env("TBA_MSVC_LIB", tc.msvc_lib.trim());
        cmd.env("TBA_SDK_LIB", tc.sdk_lib.trim());
        if !tc.extra_lib.trim().is_empty() {
            cmd.env("TBA_EXTRA_LIB", tc.extra_lib.trim());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("tba-tc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Store { root }
    }

    // 空路径要报「缺」，不能静默通过：否则编译时才炸，错误还看不懂。
    #[test]
    fn empty_paths_are_problems() {
        let p = check(&Toolchain::default());
        assert_eq!(p.len(), 3, "三个目录都该报缺: {p:?}");
        assert!(p[0].contains("link.exe"));
    }

    // 目录存在但没有那个文件 = 选错了一层目录，要说清缺哪个文件。
    #[test]
    fn wrong_dir_names_the_missing_file() {
        let t = tmp("wrong");
        let d = t.root.join("bin").display().to_string();
        std::fs::create_dir_all(&d).unwrap();
        let tc = Toolchain {
            msvc_bin: d.clone(),
            msvc_lib: d.clone(),
            sdk_lib: d.clone(),
            extra_lib: String::new(),
        };
        let p = check(&tc);
        assert!(p.iter().any(|x| x.contains("没有 link.exe")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("没有 libcmt.lib")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("没有 kernel32.lib")), "{p:?}");
    }

    // 探针齐了就算可用（不要求真的是 MSVC，验活只看文件在不在）。
    #[test]
    fn all_probes_present_is_ready() {
        let t = tmp("ready");
        let bin = t.root.join("bin");
        let lib = t.root.join("lib");
        let sdk = t.root.join("sdk");
        for (d, f) in [(&bin, "link.exe"), (&lib, "libcmt.lib"), (&sdk, "kernel32.lib")] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join(f), "").unwrap();
        }
        let tc = Toolchain {
            msvc_bin: bin.display().to_string(),
            msvc_lib: lib.display().to_string(),
            sdk_lib: sdk.display().to_string(),
            extra_lib: String::new(),
        };
        assert!(check(&tc).is_empty(), "{:?}", check(&tc));
    }

    // SDK 的 lib 可能是两段（um\x64 + ucrt\x64），任一段有 kernel32.lib 就算过。
    #[test]
    fn sdk_accepts_multi_dir() {
        let t = tmp("multi");
        let um = t.root.join("um");
        let ucrt = t.root.join("ucrt");
        std::fs::create_dir_all(&um).unwrap();
        std::fs::create_dir_all(&ucrt).unwrap();
        std::fs::write(um.join("kernel32.lib"), "").unwrap();
        let tc = Toolchain {
            msvc_bin: String::new(),
            msvc_lib: String::new(),
            sdk_lib: format!("{};{}", um.display(), ucrt.display()),
            extra_lib: String::new(),
        };
        let p = check(&tc);
        assert!(!p.iter().any(|x| x.contains("kernel32.lib")), "{p:?}");
    }

    // 手选时只改一项，另外两项不能被清空。
    #[test]
    fn set_keeps_untouched_fields() {
        let t = tmp("setkeep");
        let bin = t.root.join("bin");
        let lib = t.root.join("lib");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(bin.join("link.exe"), "").unwrap();
        std::fs::write(lib.join("libcmt.lib"), "").unwrap();
        let tc = Toolchain {
            msvc_bin: bin.display().to_string(),
            msvc_lib: lib.display().to_string(),
            sdk_lib: String::new(),
            extra_lib: String::new(),
        };
        save(&t, &tc).unwrap();

        // 只补 SDK：验活不过（没有 kernel32.lib），但原有两项要留着。
        let sdk = t.root.join("sdk");
        std::fs::create_dir_all(&sdk).unwrap();
        let err = set(&t, None, None, Some(&sdk.display().to_string()), None).unwrap_err();
        assert!(err.contains("kernel32.lib"), "{err}");
        let back = load(&t);
        assert_eq!(back.msvc_bin, tc.msvc_bin, "没传的字段不该被清空");
        assert_eq!(back.msvc_lib, tc.msvc_lib);
    }

    // 验活失败不落盘：错的路径写进去，下次编译才炸更难受。
    #[test]
    fn set_rejects_bad_path() {
        let t = tmp("setbad");
        let err = set(&t, Some("Z:\\nope"), None, None, None).unwrap_err();
        assert!(err.contains("link.exe"), "{err}");
        assert!(!path(&t).exists(), "失败不该写文件");
    }
}
