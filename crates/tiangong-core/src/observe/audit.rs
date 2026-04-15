//! 审计记录
//!
//! 对权限决策、工具执行、会话操作等关键事件进行结构化审计记录，
//! 复用 `~/.tiangong/audit.jsonl` 存储，扩展已有 AuditEntry 机制。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::app_state::audit::AuditEntry;
use crate::app_state::audit::append_audit_log;

/// 审计事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// 权限决策
    PermissionDecision,
    /// 工具执行
    ToolExecution,
    /// 信任模式变更
    TrustModeChanged,
    /// 会话创建
    SessionCreated,
    /// 会话删除
    SessionDeleted,
    /// 任务完成
    TaskCompleted,
    /// 任务失败
    TaskFailed,
    /// 任务取消
    TaskCancelled,
}

/// 结构化审计记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub event_type: AuditEventType,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub agent: Option<String>,
    pub worker_id: Option<String>,
    pub tool_name: Option<String>,
    /// 工具参数摘要（截断到 200 字符）
    pub args_summary: Option<String>,
    pub decision: Option<String>,
    pub target_scope: Option<String>,
    pub target_summary: Option<String>,
    pub result_summary: Option<String>,
    pub detail: String,
    pub success: bool,
    pub timestamp: String,
}

impl AuditRecord {
    pub fn new(event_type: AuditEventType, detail: impl Into<String>, success: bool) -> Self {
        Self {
            event_type,
            session_id: None,
            task_id: None,
            agent: None,
            worker_id: None,
            tool_name: None,
            args_summary: None,
            decision: None,
            target_scope: None,
            target_summary: None,
            result_summary: None,
            detail: detail.into(),
            success,
            timestamp: chrono::Local::now().naive_local().to_string(),
        }
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    pub fn with_worker(mut self, worker_id: impl Into<String>) -> Self {
        self.worker_id = Some(worker_id.into());
        self
    }

    pub fn with_tool(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
        self
    }

    pub fn with_args_summary(mut self, args: impl Into<String>) -> Self {
        let s: String = args.into();
        self.args_summary = Some(s.chars().take(200).collect());
        self
    }

    pub fn with_decision(mut self, decision: impl Into<String>) -> Self {
        self.decision = Some(decision.into());
        self
    }

    pub fn with_target_scope(mut self, scope: impl Into<String>) -> Self {
        self.target_scope = Some(scope.into());
        self
    }

    pub fn with_target_summary(mut self, summary: impl Into<String>) -> Self {
        let summary = summary.into();
        self.target_summary = Some(summary.chars().take(200).collect());
        self
    }

    pub fn with_result_summary(mut self, summary: impl Into<String>) -> Self {
        let summary = summary.into();
        self.result_summary = Some(summary.chars().take(200).collect());
        self
    }

    /// 写入审计日志（复用 app_state::audit 的 JSONL 追加机制）
    pub fn write(&self) {
        let action = format!("{:?}", self.event_type);
        let target = self
            .tool_name
            .as_deref()
            .or(self.task_id.as_deref())
            .or(self.session_id.as_deref())
            .unwrap_or("-");

        let mut metadata = Map::new();
        insert_metadata_string(&mut metadata, "session_id", self.session_id.as_deref());
        insert_metadata_string(&mut metadata, "task_id", self.task_id.as_deref());
        insert_metadata_string(&mut metadata, "agent", self.agent.as_deref());
        insert_metadata_string(&mut metadata, "worker_id", self.worker_id.as_deref());
        insert_metadata_string(&mut metadata, "tool_name", self.tool_name.as_deref());
        insert_metadata_string(&mut metadata, "args_summary", self.args_summary.as_deref());
        insert_metadata_string(&mut metadata, "decision", self.decision.as_deref());
        insert_metadata_string(&mut metadata, "target_scope", self.target_scope.as_deref());
        insert_metadata_string(
            &mut metadata,
            "target_summary",
            self.target_summary.as_deref(),
        );
        insert_metadata_string(
            &mut metadata,
            "result_summary",
            self.result_summary.as_deref(),
        );

        let entry = if metadata.is_empty() {
            AuditEntry::new(&action, target, &self.detail, self.success)
        } else {
            AuditEntry::new(&action, target, &self.detail, self.success)
                .with_metadata(Value::Object(metadata))
        };

        append_audit_log(&entry);
    }
}

fn insert_metadata_string(metadata: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        metadata.insert(key.to_string(), Value::String(value.to_string()));
    }
}

/// 记录权限决策
pub fn audit_permission(tool_name: &str, decision: &str, trust_mode: &str) {
    AuditRecord::new(
        AuditEventType::PermissionDecision,
        format!("decision={decision} trust_mode={trust_mode}"),
        decision != "denied",
    )
    .with_tool(tool_name)
    .with_decision(decision)
    .with_agent("core.permission")
    .write();
}

pub fn audit_permission_with_context(
    session_id: &str,
    tool_name: &str,
    decision: &str,
    trust_mode: &str,
    args_summary: Option<&str>,
    target_scope: Option<&str>,
    target_summary: Option<&str>,
) {
    let mut record = AuditRecord::new(
        AuditEventType::PermissionDecision,
        format!("decision={decision} trust_mode={trust_mode}"),
        decision != "denied",
    )
    .with_session(session_id)
    .with_tool(tool_name)
    .with_decision(decision)
    .with_agent("core.permission");

    if let Some(args_summary) = args_summary {
        record = record.with_args_summary(args_summary);
    }
    if let Some(target_scope) = target_scope {
        record = record.with_target_scope(target_scope);
    }
    if let Some(target_summary) = target_summary {
        record = record.with_target_summary(target_summary);
    }
    record.write();
}

pub fn audit_tool_execution(
    session_id: &str,
    tool_name: &str,
    success: bool,
    args_summary: Option<&str>,
    target_scope: Option<&str>,
    target_summary: Option<&str>,
    result_summary: &str,
) {
    let mut record = AuditRecord::new(AuditEventType::ToolExecution, result_summary, success)
        .with_session(session_id)
        .with_tool(tool_name)
        .with_agent("core.tool")
        .with_result_summary(result_summary);

    if let Some(args_summary) = args_summary {
        record = record.with_args_summary(args_summary);
    }
    if let Some(target_scope) = target_scope {
        record = record.with_target_scope(target_scope);
    }
    if let Some(target_summary) = target_summary {
        record = record.with_target_summary(target_summary);
    }
    record.write();
}

/// 记录信任模式变更
pub fn audit_trust_mode_changed(old: &str, new: &str) {
    AuditRecord::new(
        AuditEventType::TrustModeChanged,
        format!("{old} → {new}"),
        true,
    )
    .write();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_record_builder() {
        let record = AuditRecord::new(AuditEventType::ToolExecution, "test", true)
            .with_session("session-1")
            .with_task("task-1")
            .with_tool("read_file")
            .with_agent("core.tool")
            .with_target_scope("path")
            .with_result_summary("执行成功");

        assert_eq!(record.event_type, AuditEventType::ToolExecution);
        assert_eq!(record.session_id.as_deref(), Some("session-1"));
        assert_eq!(record.task_id.as_deref(), Some("task-1"));
        assert_eq!(record.tool_name.as_deref(), Some("read_file"));
        assert_eq!(record.agent.as_deref(), Some("core.tool"));
        assert_eq!(record.target_scope.as_deref(), Some("path"));
        assert_eq!(record.result_summary.as_deref(), Some("执行成功"));
        assert!(record.success);
    }
}
