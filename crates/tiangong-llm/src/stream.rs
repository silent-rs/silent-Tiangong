use std::pin::Pin;

use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::tool::ToolCall;
use crate::usage::TokenUsageData;

/// 统一流式事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProviderStreamEvent {
    MessageStart,
    ReasoningDelta(String),
    ReasoningSignatureDelta(String),
    TextDelta(String),
    ToolCallStart(ToolCall),
    ToolCallDelta {
        call_id: String,
        partial_json: String,
    },
    ToolCallEnd {
        call_id: String,
    },
    MessageEnd {
        stop_reason: Option<crate::response::StopReason>,
    },
    Usage(TokenUsageData),
    Error(String),
}

pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<ProviderStreamEvent, LlmError>> + Send>>;

/// 把一次性完整响应合成为流事件序列。
///
/// 服务端忽略 stream 参数返回普通 JSON 时，复用非流式解析结果，
/// 让流式消费方无差别处理。
pub(crate) fn complete_response_events(
    response: crate::response::ProviderResponse,
) -> Vec<Result<ProviderStreamEvent, LlmError>> {
    use crate::message::MessageContent;

    let mut events = vec![Ok(ProviderStreamEvent::MessageStart)];
    if let Some(reasoning) = response
        .reasoning_content
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        events.push(Ok(ProviderStreamEvent::ReasoningDelta(
            reasoning.to_string(),
        )));
    }
    for block in &response.assistant_message.content {
        match block {
            MessageContent::Text(text) if !text.trim().is_empty() => {
                events.push(Ok(ProviderStreamEvent::TextDelta(text.clone())));
            }
            // 完整参数直接放在 ToolCallStart，无需再跟 Delta 分片。
            MessageContent::ToolCall(call) => {
                events.push(Ok(ProviderStreamEvent::ToolCallStart(call.clone())));
            }
            _ => {}
        }
    }
    if let Some(usage) = response.usage {
        events.push(Ok(ProviderStreamEvent::Usage(usage)));
    }
    events.push(Ok(ProviderStreamEvent::MessageEnd {
        stop_reason: response.stop_reason,
    }));
    events
}
