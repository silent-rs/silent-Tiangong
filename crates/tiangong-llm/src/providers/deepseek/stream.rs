use futures_util::{StreamExt, stream};

use crate::error::LlmError;
use crate::stream::ProviderStreamEvent;
use crate::tool::ToolCall;

use super::error::map_deepseek_error;
use super::mapping::parse_stream_usage;

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
    match event {
        tiangong_deepseek::types::StreamEvent::ReasoningDelta(delta) => {
            vec![Ok(ProviderStreamEvent::ReasoningDelta(delta))]
        }
        tiangong_deepseek::types::StreamEvent::TextDelta(delta) => {
            vec![Ok(ProviderStreamEvent::TextDelta(delta))]
        }
        tiangong_deepseek::types::StreamEvent::ToolCallStart { id, name } => {
            vec![Ok(ProviderStreamEvent::ToolCallStart(ToolCall {
                id,
                name,
                arguments: serde_json::json!({}),
            }))]
        }
        tiangong_deepseek::types::StreamEvent::ToolCallDelta { index, arguments } => {
            vec![Ok(ProviderStreamEvent::ToolCallDelta {
                call_id: index.to_string(),
                partial_json: arguments,
            })]
        }
        tiangong_deepseek::types::StreamEvent::TextProtocolToolCall {
            id,
            name,
            arguments,
        } => vec![
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
        ],
        tiangong_deepseek::types::StreamEvent::Usage(usage) => {
            vec![Ok(ProviderStreamEvent::Usage(parse_stream_usage(&usage)))]
        }
        tiangong_deepseek::types::StreamEvent::Done => {
            vec![Ok(ProviderStreamEvent::MessageEnd)]
        }
        tiangong_deepseek::types::StreamEvent::Error(message) => {
            vec![Err(LlmError::Provider {
                provider: "deepseek",
                message,
            })]
        }
    }
}
