//! 审计记录
//!
//! 对权限决策、工具执行、会话操作等关键事件进行结构化审计记录，
//! 复用 `~/.tiangong/audit.jsonl` 存储，扩展已有 AuditEntry 机制。

use serde::{Deserialize, Serialize};

use crate::app_state::audit::append_audit_log;
use crate::app_state::audit::AuditEntry;

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
    pub tool_name: Option<String>,
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
            tool_name: None,
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

    pub fn with_tool(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
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
        append_audit_log(&AuditEntry::new(&action, target, &self.detail, self.success));
    }
}

/// 记录权限决策
pub fn audit_permission(
    tool_name: &str,
    decision: &str,
    trust_mode: &str,
) {
    AuditRecord::new(
        AuditEventType::PermissionDecision,
        format!("decision={decision} trust_mode={trust_mode}"),
        decision != "denied",
    )
    .with_tool(tool_name)
    .write();
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
            .with_tool("read_file");

        assert_eq!(record.event_type, AuditEventType::ToolExecution);
        assert_eq!(record.session_id.as_deref(), Some("session-1"));
        assert_eq!(record.task_id.as_deref(), Some("task-1"));
        assert_eq!(record.tool_name.as_deref(), Some("read_file"));
        assert!(record.success);
    }
}
