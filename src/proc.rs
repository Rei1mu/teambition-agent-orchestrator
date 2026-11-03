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
