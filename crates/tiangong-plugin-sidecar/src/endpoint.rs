//! 端点发布、读取与运行时目录解析。
//!
//! 所有 sidecar 共用的 endpoint 文件（`<service>.json`）、leader 文件
//! （`leader.lock` / `leader.json`）都落在运行时目录下。运行时目录解析遵循
//! 统一的三级优先级，确保运行时注入的数据目录始终生效：
//! 1. `TIANGONG_PLUGIN_DATA_DIR` → `join("runtime")`
//! 2. `TIANGONG_PLUGIN_ENDPOINT` 的父目录（运行时直接指定 endpoint 文件时）
//! 3. `~/.tiangong/<service>/runtime`（回退）

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use tiangong_plugin_runtime::protocol::IpcEndpoint;

/// 解析指定 service 的运行时目录（endpoint.json / leader.json / leader.lock 所在）。
pub fn runtime_dir(service: &str) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("TIANGONG_PLUGIN_DATA_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join("runtime"));
    }
    if let Some(dir) = std::env::var_os("TIANGONG_PLUGIN_ENDPOINT").filter(|v| !v.is_empty())
        && let Some(parent) = PathBuf::from(dir).parent()
    {
        return Ok(parent.to_path_buf());
    }
    Ok(home_dir()
        .ok_or_else(|| anyhow!("无法确定 HOME/USERPROFILE"))?
        .join(".tiangong")
        .join(service)
        .join("runtime"))
}

/// 解析指定 service 的 endpoint 文件路径。
///
/// 优先使用运行时注入的 `TIANGONG_PLUGIN_ENDPOINT`（整体覆盖），否则落在运行时目录下。
pub fn endpoint_path(service: &str) -> Result<PathBuf> {
    if let Some(path) =
        std::env::var_os("TIANGONG_PLUGIN_ENDPOINT").filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    Ok(runtime_dir(service)?.join(format!("{service}.json")))
}

/// 持久化 endpoint 信息到文件（原子写：临时文件 + rename）。
pub fn persist_endpoint(path: &PathBuf, endpoint: &IpcEndpoint) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 IPC runtime 目录失败: {}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(endpoint).with_context(|| "序列化 IPC endpoint 失败")?;
    std::fs::write(path, content)
        .with_context(|| format!("写入 IPC endpoint 文件失败: {}", path.display()))
}

/// leader.lock 文件路径。
pub fn leader_lock_path(service: &str) -> Result<PathBuf> {
    Ok(runtime_dir(service)?.join("leader.lock"))
}

/// leader.json 文件路径。
pub fn leader_info_path(service: &str) -> Result<PathBuf> {
    Ok(runtime_dir(service)?.join("leader.json"))
}

/// 确保运行时目录存在。
pub fn ensure_runtime_dir(service: &str) -> Result<()> {
    let dir = runtime_dir(service)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建 {service} 运行目录失败: {}", dir.display()))
}

/// 用户主目录（HOME > USERPROFILE > HOMEDRIVE+HOMEPATH）。
pub fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    // Windows 兼容：HOMEDRIVE + HOMEPATH 组合
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(d).join(p)),
        _ => None,
    }
}
