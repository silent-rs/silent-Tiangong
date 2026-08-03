//! Skill 存储路径计算（sidecar 自治，不依赖 host 注入）。

use std::path::PathBuf;

/// 解析用户主目录（兼容 POSIX 与 Windows）。
pub fn user_home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(home));
        }
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE");
    let path = std::env::var_os("HOMEPATH");
    match (drive, path) {
        (Some(d), Some(p)) if !d.is_empty() && !p.is_empty() => Some(PathBuf::from(d).join(p)),
        _ => None,
    }
}

/// 天工存储根：`~/.tiangong`。
pub fn storage_root() -> PathBuf {
    user_home_dir()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
}

/// 默认 skills 存储目录：`~/.tiangong/skills`。
pub fn default_skills_storage_dir_path() -> PathBuf {
    storage_root().join("skills")
}

/// mcp-lock.json 路径：`~/.tiangong/skills/mcp-lock.json`。
pub fn default_mcp_lock_path() -> PathBuf {
    default_skills_storage_dir_path().join("mcp-lock.json")
}
