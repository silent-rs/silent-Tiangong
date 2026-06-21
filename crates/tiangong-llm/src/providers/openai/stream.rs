//! Responses API 流式事件映射。
//!
//! Responses SSE 事件以 `type` 字段区分，解析为统一 `ProviderStreamEvent`。

use serde_json::Value;

use crate::stream::ProviderStreamEvent;
use crate::tool::ToolCall;

use super::mapping::parse_usage;

/// 解析单条 Responses 流式事件（已反序列化为 `Value`）。
///
/// 返回零到多个统一事件：文本增量、思考增量、工具调用增量、用量与结束。
pub fn parse_stream_event(
    payload: &Value,
) -> Vec<Result<ProviderStreamEvent, crate::error::LlmError>> {
    let mut events = Vec::new();
    let event_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "response.created" | "response.in_progress" => {
            events.push(Ok(ProviderStreamEvent::MessageStart));
        }
        "response.output_text.delta" => {
            if let Some(delta) = payload.get("delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                events.push(Ok(ProviderStreamEvent::TextDelta(delta.to_string())));
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(delta) = payload.get("delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                events.push(Ok(ProviderStreamEvent::ReasoningDelta(delta.to_string())));
            }
        }
        "response.function_call_arguments.delta" => {
            let call_id = payload
                .get("item_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(partial) = payload.get("delta").and_then(Value::as_str)
                && !partial.is_empty()
            {
                events.push(Ok(ProviderStreamEvent::ToolCallDelta {
                    call_id,
                    partial_json: partial.to_string(),
                }));
            }
        }
        "response.output_item.added" => {
            if let Some(item) = payload.get("item")
                && item.get("type").and_then(Value::as_str) == Some("function_call")
                && let Some(call) = parse_function_call_item(item)
            {
                events.push(Ok(ProviderStreamEvent::ToolCallStart(ToolCall {
                    id: call.call_id,
                    name: call.name,
                    arguments: serde_json::json!({}),
                })));
            }
        }
        "response.function_call_arguments.done" => {
            let item_id = payload
                .get("item_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !item_id.is_empty() {
                events.push(Ok(ProviderStreamEvent::ToolCallEnd { call_id: item_id }));
            }
        }
        "response.completed" => {
            if let Some(response) = payload.get("response")
                && let Some(usage) = response.get("usage")
            {
                events.push(Ok(ProviderStreamEvent::Usage(parse_usage(usage))));
            }
            events.push(Ok(ProviderStreamEvent::MessageEnd));
        }
        "response.failed" | "response.incomplete" => {
            let message = payload
                .get("response")
                .and_then(|r| r.get("error"))
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Responses 请求失败")
                .to_string();
            events.push(Ok(ProviderStreamEvent::Error(message)));
            events.push(Ok(ProviderStreamEvent::MessageEnd));
        }
        _ => {}
    }
    events
}

pub(super) struct ParsedFunctionCall {
    pub(super) call_id: String,
    pub(super) name: String,
}

pub(super) fn parse_function_call_item(item: &Value) -> Option<ParsedFunctionCall> {
    let call_id = item.get("call_id").and_then(Value::as_str)?.to_string();
    let name = item.get("name").and_then(Value::as_str)?.to_string();
    Some(ParsedFunctionCall { call_id, name })
}
