use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub action: String,
    pub target: String,
    pub detail: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl AuditEntry {
    pub fn new(action: &str, target: &str, detail: &str, success: bool) -> Self {
        Self {
            timestamp: chrono::Local::now().naive_local().to_string(),
            action: action.to_string(),
            target: target.to_string(),
            detail: detail.to_string(),
            success,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// 审计日志的存储根目录（`~/.tiangong`），与 app_state::repository::utils 保持一致。
fn storage_root() -> PathBuf {
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

fn audit_log_path() -> PathBuf {
    storage_root().join("audit.jsonl")
}

pub fn append_audit_log(entry: &AuditEntry) {
    let path = audit_log_path();
    if let Ok(json) = serde_json::to_string(entry)
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path)
    {
        let _ = writeln!(file, "{json}");
    }
}
