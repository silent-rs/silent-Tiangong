//! bot 存储路径自管（对齐 mcp / skill plugin 的自治模式）。
//!
//! plugin 自行计算 `~/.tiangong/bots/` 下的路径，不依赖 core 的 app_state。

use std::path::PathBuf;

/// 用户主目录（兼容 HOME / USERPROFILE / HOMEDRIVE+HOMEPATH）。
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

/// 天工存储根目录：`~/.tiangong/`（主目录不可用时回退到当前目录）。
fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

/// bot 配置目录：`~/.tiangong/bots/`。
pub fn default_bots_dir() -> PathBuf {
    storage_root().join("bots")
}

/// bot 配置文件路径：`~/.tiangong/bots/bots.json`。
pub fn default_bots_config_path() -> PathBuf {
    default_bots_dir().join("bots.json")
}

/// 单个 bot 的运行时目录：`~/.tiangong/bots/<id>/`（制品、PID、日志）。
pub fn bot_runtime_dir(id: &str) -> PathBuf {
    default_bots_dir().join(id)
}

/// bot 制品路径：`~/.tiangong/bots/<id>/bot`（Windows 下为 `bot.exe`）。
pub fn bot_artifact_path(id: &str) -> PathBuf {
    bot_runtime_dir(id).join(if cfg!(windows) { "bot.exe" } else { "bot" })
}

/// bot PID 文件路径：`~/.tiangong/bots/<id>/bot.pid`。
pub fn bot_pid_path(id: &str) -> PathBuf {
    bot_runtime_dir(id).join("bot.pid")
}

/// bot 日志文件路径：`~/.tiangong/bots/<id>/bot.log`。
pub fn bot_log_path(id: &str) -> PathBuf {
    bot_runtime_dir(id).join("bot.log")
}

/// bot 配置 schema 缓存路径：`~/.tiangong/bots/<id>/schema.json`。
///
/// 由 `bot --describe` 上报后写入，作为表单渲染、必填校验、环境变量注入的
/// 单一真相来源（对齐 `requirements.md` 的"外部适配程序"方针）。
pub fn bot_schema_path(id: &str) -> PathBuf {
    bot_runtime_dir(id).join("schema.json")
}

/// 审计日志路径：`~/.tiangong/audit.jsonl`（与 core 的 observe 对齐）。
pub fn audit_log_path() -> PathBuf {
    storage_root().join("audit.jsonl")
}
