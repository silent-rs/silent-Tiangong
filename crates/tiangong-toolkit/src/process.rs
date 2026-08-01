//! 子进程平台适配工具
//!
//! 提供跨平台的「无窗口」命令配置，主要供后台任务、命令执行等场景在
//! Windows 平台抑制控制台窗口弹出。

use std::process::Command as StdCommand;

use tokio::process::Command as TokioCommand;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub fn configure_no_window(command: &mut StdCommand) -> &mut StdCommand {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

pub fn configure_tokio_no_window(command: &mut TokioCommand) -> &mut TokioCommand {
    #[cfg(target_os = "windows")]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
