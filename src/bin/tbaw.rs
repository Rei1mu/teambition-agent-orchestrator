// 窗口启动器（tbaw.exe）：双击快捷方式走它，全程不碰控制台。
//
// 为什么需要它：tba.exe 是 console 子系统（CLI 必须能往 stdout 写），Windows 在
// **进程创建时**就给它分配控制台窗口，早于 main()。代码里只能等跑起来再关，而且
// Win11 默认终端是 Windows Terminal，那一下要等 WT 起窗再关，看着就是一个黑框弹出来；
// 刚装完被 Defender 扫盘时还会拖得更久。
//
// 这个 exe 是 GUI 子系统：永远不会有控制台。它只做一件事——用 CREATE_NO_WINDOW
// 把 `tba.exe ui` 拉起来，然后自己退出。
//
// 为什么不是把 tba.exe 直接改成 GUI 子系统：那样 PowerShell / cmd 不会等它结束，
// 输出也会丢（实测 PowerShell 29ms 就返回），CLI 就废了。两个 exe 各干一件事最稳。
#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::process::Command;

/// 不建控制台窗口（Win32 CREATE_NO_WINDOW）。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    let Ok(self_exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = self_exe.parent() else {
        return;
    };
    let exe = dir.join("tba.exe");
    if !exe.is_file() {
        // 装坏了才会走到这：不弹窗打扰，静默退出比弹个 MessageBox 好。
        return;
    }
    // 工作目录用用户主目录：跟快捷方式的 WorkingDir 一致，避免 cwd 落在安装目录里。
    let cwd = std::env::var("USERPROFILE").unwrap_or_else(|_| dir.display().to_string());
    let _ = Command::new(exe)
        .arg("ui")
        .current_dir(cwd)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}
