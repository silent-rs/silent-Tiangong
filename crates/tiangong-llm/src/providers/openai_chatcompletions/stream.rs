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
                        .filter(|s| !s.is_empty())
                        .unwrap_or_default()
                        .to_string();
                    let index = tool_call
                        .get("index")
                        .and_then(Value::as_u64)
                        .map(|i| i.to_string())
                        .unwrap_or_default();
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
                            let call_id = if !id.is_empty() { id } else { index };
                            events.push(Ok(ProviderStreamEvent::ToolCallDelta {
                                call_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unwrap_events(payload: Value) -> Vec<ProviderStreamEvent> {
        parse_stream_payload(&payload)
            .into_iter()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn parses_chat_completions_tool_call_start_and_delta_by_id() {
        let start_events = unwrap_events(serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_a",
                        "type": "function",
                        "function": { "name": "run_shell" }
                    }]
                }
            }]
        }));
        assert_eq!(start_events.len(), 1);
        assert!(matches!(
            &start_events[0],
            ProviderStreamEvent::ToolCallStart(call)
                if call.id == "call_a" && call.name == "run_shell"
        ));

        let delta_events = unwrap_events(serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": "{\"script\":\"pwd\"}" }
                    }]
                }
            }]
        }));
        assert_eq!(delta_events.len(), 1);
        assert!(matches!(
            &delta_events[0],
            ProviderStreamEvent::ToolCallDelta { call_id, partial_json }
                if call_id == "0" && partial_json == "{\"script\":\"pwd\"}"
        ));
    }
}
