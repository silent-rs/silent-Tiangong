//! 精简审计日志（从 `tiangong_core::observe` 复制，sidecar 自治）。
//!
//! 写入 `~/.tiangong/audit.jsonl`，append-only，多进程安全。

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

/// 解析存储根：优先 `TIANGONG_STORAGE_ROOT`（宿主共享），回退 `$HOME/.tiangong`。
fn resolve_storage_root() -> PathBuf {
    if let Some(root) = std::env::var("TIANGONG_STORAGE_ROOT")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(root);
    }
    super::paths::storage_root()
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub action: String,
    pub target: String,
    pub detail: String,
    pub success: bool,
}

impl AuditEntry {
    pub fn new(action: &str, target: &str, detail: &str, success: bool) -> Self {
        Self {
            timestamp: chrono::Local::now().naive_local().to_string(),
            action: action.to_string(),
            target: target.to_string(),
            detail: detail.to_string(),
            success,
        }
    }
}

/// 追加一行审计日志。写失败静默忽略（非关键路径）。
pub fn append_audit_log(entry: &AuditEntry) {
    let path = resolve_storage_root().join("audit.jsonl");
    let Ok(json) = serde_json::to_string(entry) else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let _ = file.write_all(format!("{json}\n").as_bytes());
}
