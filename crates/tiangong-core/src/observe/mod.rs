//! 观测层:审计记录 + 成本追踪。
//!
//! [`Observer`] 持有 storage_root,在 turn 开始时注入 TurnContext。
//! 审计日志写入 `{storage_root}/audit.jsonl`。

pub mod cost;

pub use cost::{
    CostSummary, RequestCost, SessionCost, TaskCost, build_session_cost, calculate_session_cost,
};

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ===== 审计条目(底层) =====

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

// ===== 审计事件类型 =====

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    PermissionDecision,
    ToolExecution,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub event_type: AuditEventType,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub args_summary: Option<String>,
    pub decision: Option<String>,
    pub result_summary: Option<String>,
    pub detail: String,
    pub success: bool,
    pub timestamp: String,
}

impl AuditRecord {
    fn new(event_type: AuditEventType, detail: impl Into<String>, success: bool) -> Self {
        Self {
            event_type,
            session_id: None,
            tool_name: None,
            args_summary: None,
            decision: None,
            result_summary: None,
            detail: detail.into(),
            success,
            timestamp: chrono::Local::now().naive_local().to_string(),
        }
    }

    fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    fn with_tool(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
        self
    }

    fn with_args_summary(mut self, args: impl Into<String>) -> Self {
        let s: String = args.into();
        self.args_summary = Some(s.chars().take(200).collect());
        self
    }

    fn with_decision(mut self, decision: impl Into<String>) -> Self {
        self.decision = Some(decision.into());
        self
    }

    fn with_result_summary(mut self, summary: impl Into<String>) -> Self {
        let summary = summary.into();
        self.result_summary = Some(summary.chars().take(200).collect());
        self
    }
}

// ===== Observer =====

/// 观测器:持有 storage_root,提供审计日志写入。
///
/// 在 turn 开始时注入 TurnContext,turn 结束时随 TurnContext 销毁。
/// audit.jsonl 路径由 storage_root 决定,不依赖全局 STORAGE_ROOT。
#[derive(Clone)]
pub struct Observer {
    pub storage_root: PathBuf,
}

impl Observer {
    pub fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }

    fn audit_path(&self) -> PathBuf {
        self.storage_root.join("audit.jsonl")
    }

    fn append(&self, entry: &AuditEntry) {
        let path = self.audit_path();
        if let Ok(json) = serde_json::to_string(entry)
            && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path)
        {
            let _ = writeln!(file, "{json}");
        }
    }

    fn write_record(&self, record: &AuditRecord) {
        let action = format!("{:?}", record.event_type);
        let target = record
            .tool_name
            .as_deref()
            .or(record.session_id.as_deref())
            .unwrap_or("-");

        let mut metadata = Map::new();
        insert_metadata(&mut metadata, "session_id", record.session_id.as_deref());
        insert_metadata(&mut metadata, "tool_name", record.tool_name.as_deref());
        insert_metadata(
            &mut metadata,
            "args_summary",
            record.args_summary.as_deref(),
        );
        insert_metadata(&mut metadata, "decision", record.decision.as_deref());
        insert_metadata(
            &mut metadata,
            "result_summary",
            record.result_summary.as_deref(),
        );

        let entry = if metadata.is_empty() {
            AuditEntry::new(&action, target, &record.detail, record.success)
        } else {
            AuditEntry::new(&action, target, &record.detail, record.success)
                .with_metadata(Value::Object(metadata))
        };

        self.append(&entry);
    }

    /// 记录权限决策
    pub fn audit_permission(
        &self,
        session_id: &str,
        tool_name: &str,
        decision: &str,
        trust_mode: &str,
        args_summary: Option<&str>,
    ) {
        let mut record = AuditRecord::new(
            AuditEventType::PermissionDecision,
            format!("decision={decision} trust_mode={trust_mode}"),
            decision != "denied",
        )
        .with_session(session_id)
        .with_tool(tool_name)
        .with_decision(decision);

        if let Some(args_summary) = args_summary {
            record = record.with_args_summary(args_summary);
        }
        self.write_record(&record);
    }

    /// 记录工具执行结果
    pub fn audit_tool_execution(
        &self,
        session_id: &str,
        tool_name: &str,
        success: bool,
        args_summary: Option<&str>,
        result_summary: &str,
    ) {
        let mut record = AuditRecord::new(AuditEventType::ToolExecution, result_summary, success)
            .with_session(session_id)
            .with_tool(tool_name)
            .with_result_summary(result_summary);

        if let Some(args_summary) = args_summary {
            record = record.with_args_summary(args_summary);
        }
        self.write_record(&record);
    }
}

fn insert_metadata(metadata: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        metadata.insert(key.to_string(), Value::String(value.to_string()));
    }
}

/// 计算默认存储根目录（`~/.tiangong`），供非 turn 级审计使用。
fn default_storage_root() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".tiangong"))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".tiangong")
        })
}

// 供插件管理操作（非 turn 级）使用的底层审计 API。
// turn 级审计经 Observer 实例写入；这些函数从 HOME 计算 storage_root。
pub fn append_audit_log(entry: &AuditEntry) {
    let path = default_storage_root().join("audit.jsonl");
    if let Ok(json) = serde_json::to_string(entry)
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
    {
        let _ = std::io::Write::write_fmt(&mut file, format_args!("{json}\n"));
    }
}
