//! 事件驱动循环运行时类型定义

use serde::{Deserialize, Serialize};

use crate::model::{ModelFunctionCall, TokenUsage};
use crate::session::Message;
use crate::tool::ToolResult;

/// 循环输入事件
///
/// 所有外部输入统一为 LoopEvent，用户消息只是其中一种。
#[derive(Debug, Clone)]
pub enum LoopEvent {
    /// 用户消息
    UserMessage { content: String },
    /// 工具执行结果（内部产生，自动回注）
    ToolResult {
        call_name: String,
        result: ToolResult,
    },
    /// 后台任务完成
    BackgroundTaskDone {
        task_id: String,
        summary: String,
        success: bool,
    },
    /// 权限审批响应
    PermissionResponse { request_id: String, approved: bool },
    /// 用户取消当前处理
    Cancel,
    /// 系统信号
    SystemSignal(SystemSignalKind),
}

/// 系统信号类型
#[derive(Debug, Clone)]
pub enum SystemSignalKind {
    /// 应用退出，需要保存状态
    Shutdown,
    /// 配置变更（MCP/Skill 热更新等）
    ConfigChanged,
}

/// 循环阶段
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "phase")]
pub enum LoopPhase {
    /// 空闲：等待事件（即将挂起）
    Idle,
    /// 处理中：组织上下文 → LLM 调用 → 工具执行
    Processing,
    /// 等待用户审批
    WaitingApproval {
        request_id: String,
        tool_name: String,
    },
    /// 已取消
    Cancelled,
}

impl LoopPhase {
    pub fn display_name(&self) -> &str {
        match self {
            Self::Idle => "空闲",
            Self::Processing => "处理中",
            Self::WaitingApproval { .. } => "等待审批",
            Self::Cancelled => "已取消",
        }
    }
}

/// 循环状态快照（可挂起/恢复/持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopState {
    /// 会话 ID
    pub session_id: String,
    /// 循环内累积的中间消息（工具结果反馈等，不含完整历史）
    pub loop_context: Vec<Message>,
    /// 累积 token 使用量
    pub accumulated_usage: TokenUsage,
    /// 当前轮次
    pub round: usize,
    /// 待处理的工具调用（被中断时可能有）
    pub pending_tool_calls: Vec<PendingToolCall>,
    /// 被中断时的阶段
    pub interrupted_phase: LoopPhase,
    /// 挂起时间
    pub suspended_at: Option<String>,
}

/// 待处理的工具调用（持久化用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

impl From<&ModelFunctionCall> for PendingToolCall {
    fn from(call: &ModelFunctionCall) -> Self {
        Self {
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        }
    }
}

impl LoopState {
    /// 创建新的循环状态
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            loop_context: Vec::new(),
            accumulated_usage: TokenUsage::default(),
            round: 0,
            pending_tool_calls: Vec::new(),
            interrupted_phase: LoopPhase::Idle,
            suspended_at: None,
        }
    }

    /// 标记为挂起
    pub fn mark_suspended(&mut self) {
        self.suspended_at = Some(chrono::Local::now().naive_local().to_string());
    }
}

/// 循环运行结果
#[derive(Debug)]
pub enum LoopOutcome {
    /// 正常挂起（无事件，等待下次唤起）
    Suspended(LoopState),
    /// 被取消
    Cancelled(LoopState),
    /// 收到关闭信号，需要持久化
    Shutdown(LoopState),
    /// 执行出错
    Error(LoopState, String),
}

impl LoopOutcome {
    /// 提取内部的 LoopState
    pub fn into_state(self) -> LoopState {
        match self {
            Self::Suspended(s) | Self::Cancelled(s) | Self::Shutdown(s) | Self::Error(s, _) => s,
        }
    }
}

/// EventLoop 向外部输出的事件（复用现有 TurnEvent）
pub use crate::app_state::TurnEvent as OutputEvent;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_state_new() {
        let state = LoopState::new("s1".into());
        assert_eq!(state.session_id, "s1");
        assert_eq!(state.round, 0);
        assert!(state.loop_context.is_empty());
        assert!(state.suspended_at.is_none());
    }

    #[test]
    fn loop_state_mark_suspended() {
        let mut state = LoopState::new("s1".into());
        state.mark_suspended();
        assert!(state.suspended_at.is_some());
    }

    #[test]
    fn loop_phase_display() {
        assert_eq!(LoopPhase::Idle.display_name(), "空闲");
        assert_eq!(LoopPhase::Processing.display_name(), "处理中");
    }

    #[test]
    fn loop_state_serialization() {
        let state = LoopState::new("s1".into());
        let json = serde_json::to_string(&state).unwrap();
        let parsed: LoopState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "s1");
    }

    #[test]
    fn loop_outcome_into_state() {
        let state = LoopState::new("s1".into());
        let outcome = LoopOutcome::Suspended(state);
        let s = outcome.into_state();
        assert_eq!(s.session_id, "s1");
    }
}
