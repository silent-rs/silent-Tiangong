//! Skill 存储路径自管。
//!
//! Skill 相关路径由本 plugin 自行计算（`~/.tiangong/skills/` 下），不再依赖
//! core 的路径函数——Skill 概念已从 core 迁出。

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

/// 天工存储根目录：`~/.tiangong/`。
pub(crate) fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

/// Skill 存储目录：`~/.tiangong/skills/`。
pub fn default_skills_storage_dir_path() -> PathBuf {
    storage_root().join("skills")
}

/// MCP 依赖锁路径：`~/.tiangong/skills/mcp-lock.json`。
pub fn default_mcp_lock_path() -> PathBuf {
    default_skills_storage_dir_path().join("mcp-lock.json")
}
