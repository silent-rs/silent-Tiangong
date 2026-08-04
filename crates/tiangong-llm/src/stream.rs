use std::pin::Pin;

use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::response::StopReason;
use crate::tool::ToolCall;
use crate::usage::TokenUsageData;

/// 统一流式事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProviderStreamEvent {
    MessageStart,
    ReasoningDelta(String),
    ReasoningSignatureDelta(String),
    TextDelta(String),
    ToolCallStart {
        index: usize,
        call: ToolCall,
    },
    ToolCallDelta {
        index: usize,
        call_id: String,
        partial_json: String,
    },
    ToolCallEnd {
        index: usize,
        call_id: String,
    },
    FinishReason(StopReason),
    MessageEnd,
    Usage(TokenUsageData),
    Error(String),
}

pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<ProviderStreamEvent, LlmError>> + Send>>;
