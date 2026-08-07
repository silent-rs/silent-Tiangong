//! MCP 存储路径自管（对齐 skill plugin 的自治模式）。
//!
//! plugin 从 App 注入的存储根目录计算 MCP 相关路径，不依赖 core 的
//! `app_state::default_mcp_*` 路径函数——MCP 概念已从 core 彻底迁出。

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

/// 天工存储根目录：优先使用 App 注入值，否则回退 `~/.tiangong/`。
fn storage_root() -> PathBuf {
    if let Some(root) = std::env::var_os(tiangong_plugin_runtime::sidecar::STORAGE_ROOT_ENV)
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(root);
    }
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

/// MCP 配置文件路径：`~/.tiangong/mcp.json`。
pub fn default_mcp_config_path() -> PathBuf {
    storage_root().join("mcp.json")
}

/// MCP tools 缓存路径：`~/.tiangong/mcp-tools-cache.json`。
pub fn default_mcp_capability_cache_path() -> PathBuf {
    storage_root().join("mcp-tools-cache.json")
}

/// MCP 依赖锁路径：`~/.tiangong/skills/mcp-lock.json`（与 skills 存储同目录）。
#[allow(dead_code)]
pub fn default_mcp_lock_path() -> PathBuf {
    storage_root().join("skills").join("mcp-lock.json")
}
