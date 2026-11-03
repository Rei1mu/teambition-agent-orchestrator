// 通用能力：内置重载流程（编译 -> 杀旧 -> 复制校验 -> 启动）。
//
// 设计原则（对应 README「项目目标」第 3 条）：
// 关键流程不靠文字 skill 描述，而是固化成命令。agent 只调用，行为由脚本保证：
// 退出码是唯一真值，cargo 输出全进日志，不回灌上下文。

use std::path::{Path, PathBuf};
use std::process::Command;

// 脚本随二进制走，运行时释放到 data/capabilities/。
pub const RELOAD_PS1: &str = include_str!("../assets/reload.ps1");

pub fn caps_dir() -> PathBuf {
    crate::config::data_dir().join("capabilities")
}

pub fn status_file() -> PathBuf {
    crate::config::data_dir().join("reload.status")
}

// 释放/更新内置脚本。内容不一致就重写，保证用的是当前二进制里的版本。
pub fn ensure_reload_script() -> Result<PathBuf, String> {
    let dir = caps_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建能力目录失败: {e}"))?;
    let path = dir.join("reload.ps1");
    let stale = match std::fs::read_to_string(&path) {
        Ok(s) => s != RELOAD_PS1,
        Err(_) => true,
    };
    if stale {
        std::fs::write(&path, RELOAD_PS1).map_err(|e| format!("写入 reload.ps1 失败: {e}"))?;
    }
    Ok(path)
}

pub struct Outcome {
    pub code: i32,
    pub out: String,
    pub err: String,
}

// 一次重载请求。参数多，用结构体传：以前 8 个位置参数，加个编译指令
// 就得改所有调用点，还容易传错顺序。
pub struct ReloadReq {
    pub action: String,
    pub root: String,
    pub name: String,
    pub live: String,
    pub args: String,
    pub mode: String,
    /// 编译指令。空 = 走脚本里的默认（cargo）。
    pub compile: String,
    pub elevate: bool,
    pub release_src: bool,
}

fn build_cmd(req: &ReloadReq) -> Result<Command, String> {
    let script = ensure_reload_script()?;
    let mut cmd = Command::new("powershell.exe");
    cmd.arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(script);
    cmd.arg("-Action").arg(&req.action);
    cmd.arg("-Root").arg(&req.root);
    cmd.arg("-StatusFile").arg(status_file());
    cmd.arg("-Mode").arg(&req.mode);
    // 调用方进程永不杀（rename 模式下调用方就是产物本身）
    cmd.arg("-SelfPid").arg(std::process::id().to_string());
    if !req.name.trim().is_empty() {
        cmd.arg("-Name").arg(req.name.trim());
    }
    if !req.live.trim().is_empty() {
        cmd.arg("-Live").arg(req.live.trim());
    }
    if !req.args.trim().is_empty() {
        cmd.arg("-LaunchArgs").arg(req.args.trim());
    }
    // 编译指令来自项目设置（Teambition 的 setting 任务）：rust 是 cargo，
    // zig / go / npm 各不相同，写死在脚本里只能覆盖一种。
    if !req.compile.trim().is_empty() {
        cmd.arg("-Compile").arg(req.compile.trim());
    }
    if req.release_src {
        cmd.arg("-ReleaseSrc");
    }
    if req.elevate {
        cmd.arg("-Elevate");
    }
    // 别弹黑窗：编译要几十秒，一个控制台窗口杵在那儿最碍事。
    crate::proc::hide(&mut cmd);
    Ok(cmd)
}

// 同步执行：脚本里完成 编译 -> 杀旧 -> (复制校验) -> 启动，退出码即结果。
// mode: "rename" 改名直跑 / "copy" 副本运行。
pub fn run_sync(req: &ReloadReq) -> Result<Outcome, String> {
    let mut cmd = build_cmd(req)?;
    let out = cmd
        .output()
        .map_err(|e| format!("启动 reload.ps1 失败: {e}"))?;
    Ok(Outcome {
        code: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        err: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

// 当前进程是不是 cargo 产物（target/release/<name>.exe）。
// 是的话编译前必须先把它重命名挪开，否则链接器写不进去。
pub fn is_src(root: &str, name: &str) -> bool {
    let cur = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let src = Path::new(root)
        .join("target")
        .join("release")
        .join(format!("{name}.exe"));
    same_file(&cur, &src)
}

// 当前进程是不是运行副本。是的话脚本杀旧会把自己杀掉，属预期行为。
#[allow(dead_code)]
pub fn is_live(live: &str) -> bool {
    let cur = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    same_file(&cur, Path::new(live))
}

#[allow(dead_code)] // 窗口的重载页搬进项目卡片后暂时没有调用点，留着给 CLI 排查
pub fn last_status() -> String {
    std::fs::read_to_string(status_file())
        .unwrap_or_default()
        .trim_start_matches('\u{feff}')
        .trim()
        .to_string()
}

// reload.log 末尾若干行，给窗口展示用。
// PowerShell 的重定向可能是 UTF-16LE，按 BOM 判断解码。
pub fn log_tail(root: &str, lines: usize) -> String {
    let p = Path::new(root).join("target").join("reload.log");
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    let text = if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

// 猜项目根：exe 在 target/release/ 下就往上两级。
pub fn guess_root() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir
                .file_name()
                .map(|n| n.eq_ignore_ascii_case("release"))
                .unwrap_or(false)
            {
                if let Some(target) = dir.parent() {
                    if target
                        .file_name()
                        .map(|n| n.eq_ignore_ascii_case("target"))
                        .unwrap_or(false)
                    {
                        if let Some(root) = target.parent() {
                            return root.display().to_string();
                        }
                    }
                }
            }
            return dir.display().to_string();
        }
    }
    ".".to_string()
}

// 从 Cargo.toml 猜 exe 基名。优先级：[[bin]] name > default-run > [package] name > 目录名。
// 注意 [package] name 不一定是产物名（tba 的产物是 tb）。
pub fn guess_name(root: &str) -> String {
    let toml = Path::new(root).join("Cargo.toml");
    if let Ok(s) = std::fs::read_to_string(&toml) {
        let (mut pkg, mut bin, mut default_run) = (String::new(), String::new(), String::new());
        let mut in_bin = false;
        for line in s.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_bin = t == "[[bin]]";
                continue;
            }
            if let Some((k, v)) = t.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('"').trim_matches('\'');
                match k {
                    "name" if in_bin && bin.is_empty() => bin = v.to_string(),
                    "name" if !in_bin && pkg.is_empty() => pkg = v.to_string(),
                    "default-run" if default_run.is_empty() => default_run = v.to_string(),
                    _ => {}
                }
            }
        }
        for cand in [bin, default_run, pkg] {
            if !cand.is_empty() {
                return cand;
            }
        }
    }
    Path::new(root)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "app".to_string())
}

// 运行副本放在 cargo 产物同目录，保证 data/ 只有一份
// （data_dir 跟 exe 走，副本放别处会导致 token/台账分家）。
pub fn default_live(root: &str, name: &str) -> String {
    Path::new(root)
        .join("target")
        .join("release")
        .join(format!("{name}_test.exe"))
        .display()
        .to_string()
}
