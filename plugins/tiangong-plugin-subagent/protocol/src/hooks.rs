//! Hook 事件：Subagent 的重要反馈投影回激活会话。
//!
//! 可靠性约定：事件先持久化（`hooks/queue/`），再尝试投递；每个事件有唯一
//! ID，接收端按 ID 去重；投递失败重试；sidecar 重启后继续投递未送达事件。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventType {
    /// 普通消息（有价值的输出摘要，非高频进度）。
    Message,
    /// 阻塞，等待外部输入。
    Blocked,
    /// 需要审批。
    ApprovalRequired,
    /// 完成。
    Completed,
    /// 失败。
    Failed,
}

impl HookEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Blocked => "blocked",
            Self::ApprovalRequired => "approval_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// 待投递 Hook 事件（投递成功后从队列移除，历史保留在事件日志）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEvent {
    pub event_id: String,
    pub agent_id: String,
    pub agent_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_id: Option<String>,
    /// 目标会话（conversation）。
    pub conversation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub workspace: String,
    pub event_type: HookEventType,
    pub created_at: String,
    pub payload: serde_json::Value,
    /// 投递尝试次数。
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl HookEvent {
    /// 投递到会话的文本形态（经 server 消息通道注入为用户可见消息）。
    pub fn render_message(&self) -> String {
        let prefix = format!("[Subagent·{}]", self.agent_name);
        match self.event_type {
            HookEventType::Message => format!("{prefix} {}", self.payload_text()),
            HookEventType::Blocked => format!(
                "{prefix} 运行阻塞：{}。可回复补充指示，或用 interrupt_agent_run / cancel_agent_run 控制。",
                self.payload_text()
            ),
            HookEventType::ApprovalRequired => {
                format!("{prefix} 等待审批：{}", self.payload_text())
            }
            HookEventType::Completed => format!("{prefix} 任务完成：{}", self.payload_text()),
            HookEventType::Failed => {
                format!("{prefix} 执行失败：{}", self.payload_text())
            }
        }
    }

    fn payload_text(&self) -> String {
        if let Some(text) = self.payload.get("text").and_then(|v| v.as_str()) {
            return text.to_string();
        }
        if self.payload.is_null() || self.payload.as_object().is_none_or(|m| m.is_empty()) {
            return String::new();
        }
        serde_json::to_string(&self.payload).unwrap_or_default()
    }
}
