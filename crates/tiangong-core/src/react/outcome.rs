//! 单轮 Agent Loop 的执行结果。

use crate::model::TokenUsage;
use tiangong_types::{StreamEvent, TurnStatus};

#[derive(Debug)]
pub(super) enum TurnExecutionOutcome {
    Success,
    Cancelled,
    Failed(String),
}

impl TurnExecutionOutcome {
    pub(super) fn status(&self) -> TurnStatus {
        match self {
            Self::Success => TurnStatus::Success,
            Self::Cancelled => TurnStatus::Cancelled,
            Self::Failed(_) => TurnStatus::Failed,
        }
    }

    pub(super) fn terminal_event(&self, usage: TokenUsage) -> StreamEvent {
        match self {
            Self::Success => StreamEvent::Done { usage: Some(usage) },
            Self::Cancelled => StreamEvent::Error {
                message: "已取消".to_string(),
            },
            Self::Failed(message) => StreamEvent::Error {
                message: message.clone(),
            },
        }
    }
}

#[derive(Debug)]
pub(super) struct TurnExecutionResult {
    pub(super) usage: TokenUsage,
    pub(super) outcome: TurnExecutionOutcome,
    /// 成功路径本轮候选答复（已标记 Summary）的消息 ID：run_turn 收尾降级时
    /// 按 ID 精确回收，不依赖倒序查找——插件在 on_turn_finished 中追加或修改
    /// Summary 相位时也不会误伤。
    pub(super) finalized_candidate_id: Option<String>,
}

impl TurnExecutionResult {
    pub(super) fn success(usage: TokenUsage) -> Self {
        Self {
            usage,
            outcome: TurnExecutionOutcome::Success,
            finalized_candidate_id: None,
        }
    }

    pub(super) fn cancelled(usage: TokenUsage) -> Self {
        Self {
            usage,
            outcome: TurnExecutionOutcome::Cancelled,
            finalized_candidate_id: None,
        }
    }

    pub(super) fn failed(usage: TokenUsage, message: impl Into<String>) -> Self {
        Self {
            usage,
            outcome: TurnExecutionOutcome::Failed(message.into()),
            finalized_candidate_id: None,
        }
    }
}
