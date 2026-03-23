use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

use super::repository::default_storage_root;

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

fn audit_log_path() -> PathBuf {
    default_storage_root().join("audit.jsonl")
}

pub fn append_audit_log(entry: &AuditEntry) {
    let path = audit_log_path();
    if let Ok(json) = serde_json::to_string(entry)
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path)
    {
        let _ = writeln!(file, "{json}");
    }
}
