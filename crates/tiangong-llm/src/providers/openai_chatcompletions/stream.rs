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
                for (position, tool_call) in tool_calls.iter().enumerate() {
                    let id = tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_default()
                        .to_string();
                    let index = tool_call
                        .get("index")
                        .and_then(Value::as_u64)
                        .and_then(|value| usize::try_from(value).ok())
                        .unwrap_or(position);
                    if let Some(function) = tool_call.get("function") {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            events.push(Ok(ProviderStreamEvent::ToolCallStart {
                                index,
                                call: crate::tool::ToolCall {
                                    id: id.clone(),
                                    name: name.to_string(),
                                    arguments: serde_json::json!({}),
                                },
                            }));
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str)
                            && !arguments.is_empty()
                        {
                            events.push(Ok(ProviderStreamEvent::ToolCallDelta {
                                index,
                                call_id: id,
                                partial_json: arguments.to_string(),
                            }));
                        }
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                events.push(Ok(ProviderStreamEvent::FinishReason(
                    super::mapping::map_stop_reason(reason),
                )));
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
            ProviderStreamEvent::ToolCallStart { index, call }
                if *index == 0 && call.id == "call_a" && call.name == "run_shell"
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
            ProviderStreamEvent::ToolCallDelta { index, call_id, partial_json }
                if *index == 0 && call_id.is_empty()
                    && partial_json == "{\"script\":\"pwd\"}"
        ));
    }

    #[test]
    fn preserves_parallel_indexes_and_finish_reason() {
        let events = unwrap_events(serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        {
                            "index": 1,
                            "id": "call_b",
                            "function": { "name": "read_file", "arguments": "{\"path\":\"b\"}" }
                        },
                        {
                            "index": 0,
                            "id": "call_a",
                            "function": { "name": "read_file", "arguments": "{\"path\":\"a\"}" }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        }));

        assert!(events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::ToolCallStart { index: 1, call }
                if call.id == "call_b"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::ToolCallDelta { index: 0, call_id, .. }
                if call_id == "call_a"
        )));
        assert!(events.contains(&ProviderStreamEvent::FinishReason(
            crate::response::StopReason::ToolUse
        )));
    }
}
