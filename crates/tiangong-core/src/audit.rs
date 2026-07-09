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

fn audit_log_path() -> PathBuf {
    crate::storage::storage_root().join("audit.jsonl")
}

pub fn append_audit_log(entry: &AuditEntry) {
    let path = audit_log_path();
    if let Ok(json) = serde_json::to_string(entry)
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path)
    {
        let _ = writeln!(file, "{json}");
    }
}
