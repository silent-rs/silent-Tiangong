//! 平台适配工具：子进程「无窗口」配置与应用自管存储根。
//!
//! 本模块与 `tiangong-toolkit` 的同名实现是经认可的双份副本（core 为解除对
//! toolkit 的依赖而自带一份）：`configure_no_window` 供 plugin-runtime 等
//! 依赖 core 的上层 crate 复用；`app_storage_root` 供系统提示词描述
//! 「允许文件操作目录」，必须与 toolkit 写边界校验所放行的目录保持一致，
//! **改动任一份时必须同步另一份**。

use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 跨平台的「无窗口」命令配置：Windows 上抑制子进程控制台窗口弹出，其余平台空操作。
pub fn configure_no_window(command: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// 应用自管存储根目录：`~/.tiangong/`。
///
/// 与 toolkit 的写边界校验同源（见模块文档的双份同步约定）。
pub fn app_storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = PathBuf::from(drive);
            buf.push(path);
            Some(buf)
        }
        _ => None,
    }
}
