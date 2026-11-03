// 起子进程统一走这里：Windows 上永远不弹控制台窗口。
//
// 为什么必须有这个：tba 自己是控制台子系统（CLI 要能直接输出、agent 要能读
// stdout），从窗口里起 git / but / cargo / powershell 时，子进程会各自分配一个
// 黑窗口，闪一下就走，用起来像中毒。CREATE_NO_WINDOW 让子进程不分配控制台，
// 输出照样能通过管道读回来（我们本来就不用它的窗口）。
//
// 注意：这只影响子进程。tba 自己的控制台在 GUI 模式由 ui::hide_console 释放。

use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 给命令加上「别弹窗」标志。链式用：`let mut c = Command::new("git"); proc::hide(&mut c);`
pub fn hide(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// 用系统默认程序打开一个链接（默认浏览器）。
///
/// 以前窗口里和 OAuth 流程都用 `cmd /C start "" <url>`：在 GUI 进程里这条路不稳——
/// `start` 对参数和宿主很挑，有的机器上直接什么都不做，点了按钮毫无反应也看不到报错。
/// 改成直接调 shell32 的 ShellExecuteW：「用默认程序打开」的官方入口，
/// 默认浏览器/关联程序交给系统决定，也不用起子进程。
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide = |s: &str| -> Vec<u16> {
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        };
        let op = wide("open");
        let file = wide(url);
        // SW_SHOWNORMAL = 1
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        // Win32 老约定：<= 32 就是失败。失败再退回 cmd start 试一次——
        // 两条路都不通至少不会把窗口搞崩。
        if r > 32 {
            return;
        }
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        hide(&mut c);
        let _ = c.spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: *mut std::ffi::c_void,
        op: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> isize;
}
