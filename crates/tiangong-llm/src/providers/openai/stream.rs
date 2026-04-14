use serde_json::Value;

use crate::stream::ProviderStreamEvent;

pub fn parse_stream_payload(
    payload: &Value,
) -> Vec<Result<ProviderStreamEvent, crate::error::LlmError>> {
    let mut events = Vec::new();
    if let Some(usage) = payload.get("usage") {
        events.push(Ok(ProviderStreamEvent::Usage(super::mapping::parse_usage(
            usage,
        ))));
    }
    if let Some(choices) = payload.get("choices").and_then(Value::as_array) {
        for choice in choices {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str)
                && !reasoning.is_empty()
            {
                events.push(Ok(ProviderStreamEvent::ReasoningDelta(
                    reasoning.to_string(),
                )));
            }
            if let Some(content) = delta.get("content").and_then(Value::as_str)
                && !content.is_empty()
            {
                events.push(Ok(ProviderStreamEvent::TextDelta(content.to_string())));
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    let id = tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if let Some(function) = tool_call.get("function") {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            events.push(Ok(ProviderStreamEvent::ToolCallStart(
                                crate::tool::ToolCall {
                                    id: id.clone(),
                                    name: name.to_string(),
                                    arguments: serde_json::json!({}),
                                },
                            )));
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str)
                            && !arguments.is_empty()
                        {
                            events.push(Ok(ProviderStreamEvent::ToolCallDelta {
                                call_id: id.clone(),
                                partial_json: arguments.to_string(),
                            }));
                        }
                    }
                }
            }
        }
    }
    events
}
