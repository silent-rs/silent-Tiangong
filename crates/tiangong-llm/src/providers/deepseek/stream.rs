use futures_util::{StreamExt, stream};

use crate::error::LlmError;
use crate::stream::ProviderStreamEvent;
use crate::tool::ToolCall;

use super::error::map_deepseek_error;
use super::mapping::parse_stream_usage;

/// 把 SDK 的 EventStream 机械映射为统一的 ProviderStreamEvent 流。
///
/// 文本工具调用协议的兜底解析已下沉到 SDK（`tiangong_deepseek::chat` 内置缓冲），
/// 本函数只做事件类型转换，不含 DeepSeek 特有逻辑。
pub fn map_deepseek_stream(
    event_stream: tiangong_deepseek::types::EventStream,
) -> super::client::DeepSeekStream {
    let mapped = event_stream.map(|result| match result {
        Ok(event) => map_event(event),
        Err(err) => vec![Err(map_deepseek_error(err))],
    });
    Box::pin(mapped.flat_map(stream::iter))
}

fn map_event(
    event: tiangong_deepseek::types::StreamEvent,
) -> Vec<Result<ProviderStreamEvent, LlmError>> {
    use tiangong_deepseek::types::StreamEvent as E;
    match event {
        E::ReasoningDelta(delta) => vec![Ok(ProviderStreamEvent::ReasoningDelta(delta))],
        E::TextDelta(delta) => vec![Ok(ProviderStreamEvent::TextDelta(delta))],
        E::ToolCallStart { id, name } => {
            vec![Ok(ProviderStreamEvent::ToolCallStart(ToolCall {
                id,
                name,
                arguments: serde_json::json!({}),
            }))]
        }
        E::ToolCallDelta { index, arguments } => {
            vec![Ok(ProviderStreamEvent::ToolCallDelta {
                call_id: index.to_string(),
                partial_json: arguments,
            })]
        }
        E::TextProtocolToolCall {
            id,
            name,
            arguments,
        } => {
            // SDK 文本协议兜底解析出的完整工具调用，映射为 Start→Delta→End 三件套，
            // 与结构化流式 tool_calls 的消费路径保持一致。
            vec![
                Ok(ProviderStreamEvent::ToolCallStart(ToolCall {
                    id: id.clone(),
                    name,
                    arguments: serde_json::json!({}),
                })),
                Ok(ProviderStreamEvent::ToolCallDelta {
                    call_id: id.clone(),
                    partial_json: arguments,
                }),
                Ok(ProviderStreamEvent::ToolCallEnd { call_id: id }),
            ]
        }
        E::Usage(usage) => vec![Ok(ProviderStreamEvent::Usage(parse_stream_usage(&usage)))],
        E::Done => vec![Ok(ProviderStreamEvent::MessageEnd)],
        E::Error(message) => vec![Err(LlmError::Provider {
            provider: "deepseek",
            message,
        })],
    }
}
