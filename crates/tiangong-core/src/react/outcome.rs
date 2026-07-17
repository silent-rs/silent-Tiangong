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
}

impl TurnExecutionResult {
    pub(super) fn success(usage: TokenUsage) -> Self {
        Self {
            usage,
            outcome: TurnExecutionOutcome::Success,
        }
    }

    pub(super) fn cancelled(usage: TokenUsage) -> Self {
        Self {
            usage,
            outcome: TurnExecutionOutcome::Cancelled,
        }
    }

    pub(super) fn failed(usage: TokenUsage, message: impl Into<String>) -> Self {
        Self {
            usage,
            outcome: TurnExecutionOutcome::Failed(message.into()),
        }
    }
}
