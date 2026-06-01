use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::message::{ChatMessage, MessageContent, MessageRole};
use crate::request::{ProviderRequest, ReasoningEffort};
use crate::response::{ProviderResponse, StopReason};
use crate::tool::{ToolChoice, ToolSpec};
use crate::usage::TokenUsageData;

pub fn to_deepseek_request(
    req: &ProviderRequest,
) -> Result<tiangong_deepseek::types::ChatCompletionRequest> {
    let messages = build_messages(req)?;
    let thinking = build_thinking(req);
    let reasoning_effort = build_reasoning_effort(req);

    let mut tool_choice_value = None;
    let mut tools_value = None;
    if !req.tools.is_empty() {
        tools_value = Some(build_tools(&req.tools));
        tool_choice_value = build_tool_choice(req.tool_choice.as_ref());
    }

    Ok(tiangong_deepseek::types::ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        max_tokens: Some(req.max_tokens),
        temperature: req.temperature,
        top_p: req.top_p,
        stream: None,
        stream_options: None,
        stop: if req.stop_sequences.is_empty() {
            None
        } else {
            Some(json!(req.stop_sequences))
        },
        tools: tools_value,
        tool_choice: tool_choice_value,
        thinking,
        reasoning_effort,
        response_format: None,
        user_id: None,
    })
}

fn build_messages(req: &ProviderRequest) -> Result<Vec<tiangong_deepseek::types::ChatMessage>> {
    let mut messages = Vec::new();

    if let Some(system) = req.system.as_ref().filter(|s| !s.trim().is_empty()) {
        messages.push(tiangong_deepseek::types::ChatMessage {
            role: tiangong_deepseek::types::MessageRole::System,
            content: Some(Value::String(system.clone())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            prefix: false,
        });
    }

    for message in &req.messages {
        match message.role {
            MessageRole::System => {}
            MessageRole::User => {
                if let Some(msg) = build_user_message(message) {
                    messages.push(msg);
                }
            }
            MessageRole::Assistant => {
                let text = extract_text(message);
                let tool_calls = build_assistant_tool_calls(message);
                if !text.is_empty() || !tool_calls.is_empty() {
                    messages.push(tiangong_deepseek::types::ChatMessage {
                        role: tiangong_deepseek::types::MessageRole::Assistant,
                        content: if text.is_empty() {
                            None
                        } else {
                            Some(Value::String(text))
                        },
                        name: None,
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                        tool_call_id: None,
                        prefix: false,
                    });
                }
            }
            MessageRole::Tool => {
                for result in message.content.iter().filter_map(|c| match c {
                    MessageContent::ToolResult(r) => Some(r),
                    _ => None,
                }) {
                    let content = match &result.content {
                        crate::tool::ToolResultContent::Text(text) => text.clone(),
                        crate::tool::ToolResultContent::Json(value) => value.to_string(),
                    };
                    messages.push(tiangong_deepseek::types::ChatMessage {
                        role: tiangong_deepseek::types::MessageRole::Tool,
                        content: Some(Value::String(content)),
                        name: None,
                        tool_calls: None,
                        tool_call_id: Some(result.tool_call_id.clone()),
                        prefix: false,
                    });
                }
            }
        }
    }

    Ok(messages)
}

fn build_user_message(message: &ChatMessage) -> Option<tiangong_deepseek::types::ChatMessage> {
    let text = extract_text(message);
    let images: Vec<_> = message
        .content
        .iter()
        .filter_map(|c| match c {
            MessageContent::Image(img) => Some(img),
            _ => None,
        })
        .collect();

    if images.is_empty() {
        if text.trim().is_empty() {
            return None;
        }
        return Some(tiangong_deepseek::types::ChatMessage {
            role: tiangong_deepseek::types::MessageRole::User,
            content: Some(Value::String(text)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            prefix: false,
        });
    }

    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    for image in images {
        content.push(json!({
            "type": "image_url",
            "image_url": { "url": image.data }
        }));
    }
    Some(tiangong_deepseek::types::ChatMessage {
        role: tiangong_deepseek::types::MessageRole::User,
        content: Some(Value::Array(content)),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        prefix: false,
    })
}

fn build_assistant_tool_calls(message: &ChatMessage) -> Vec<tiangong_deepseek::types::ToolCall> {
    message
        .content
        .iter()
        .filter_map(|c| match c {
            MessageContent::ToolCall(tc) => Some(tiangong_deepseek::types::ToolCall {
                id: tc.id.clone(),
                kind: "function".to_string(),
                function: tiangong_deepseek::types::FunctionCall {
                    name: tc.name.clone(),
                    arguments: tc.arguments.to_string(),
                },
            }),
            _ => None,
        })
        .collect()
}

fn build_tools(specs: &[ToolSpec]) -> Vec<tiangong_deepseek::types::ToolSpec> {
    specs
        .iter()
        .map(|spec| tiangong_deepseek::types::ToolSpec {
            kind: "function".to_string(),
            function: tiangong_deepseek::types::FunctionSpec {
                name: spec.name.clone(),
                description: Some(spec.description.clone()),
                parameters: Some(spec.input_schema.clone()),
                strict: false,
            },
        })
        .collect()
}

fn build_tool_choice(choice: Option<&ToolChoice>) -> Option<Value> {
    match choice {
        Some(ToolChoice::Any) => Some(json!("required")),
        Some(ToolChoice::Auto) => Some(json!("auto")),
        Some(ToolChoice::Tool(name)) => Some(json!({
            "type": "function",
            "function": { "name": name }
        })),
        None => None,
    }
}

fn build_thinking(req: &ProviderRequest) -> Option<tiangong_deepseek::types::ThinkingConfig> {
    if req.thinking_disabled {
        Some(tiangong_deepseek::types::ThinkingConfig {
            thinking_type: "disabled".to_string(),
            budget_tokens: None,
        })
    } else {
        req.thinking
            .as_ref()
            .map(|thinking| tiangong_deepseek::types::ThinkingConfig {
                thinking_type: "enabled".to_string(),
                budget_tokens: Some(thinking.budget_tokens),
            })
    }
}

fn build_reasoning_effort(
    req: &ProviderRequest,
) -> Option<tiangong_deepseek::types::ReasoningEffort> {
    req.reasoning_effort.map(|effort| match effort {
        ReasoningEffort::Low | ReasoningEffort::Medium | ReasoningEffort::High => {
            tiangong_deepseek::types::ReasoningEffort::High
        }
        ReasoningEffort::Max => tiangong_deepseek::types::ReasoningEffort::Max,
    })
}

fn extract_text(message: &ChatMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|c| match c {
            MessageContent::Text(text) => Some(text.clone()),
            MessageContent::ToolResult(result) => Some(match &result.content {
                crate::tool::ToolResultContent::Text(text) => text.clone(),
                crate::tool::ToolResultContent::Json(value) => value.to_string(),
            }),
            MessageContent::File(file) => Some(format!(
                "<attachment title=\"{}\" mime_type=\"{}\">\n{}\n</attachment>",
                file.title.as_deref().unwrap_or("attachment"),
                file.mime_type,
                file.data
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn from_deepseek_response(
    response: tiangong_deepseek::types::ChatCompletionResponse,
) -> Result<ProviderResponse> {
    let choice = response
        .choices
        .first()
        .ok_or_else(|| anyhow!("DeepSeek 响应缺少 choices"))?;

    let text = choice.message.content.as_deref().unwrap_or("").to_string();
    let reasoning_content = choice.message.reasoning_content.clone();

    let mut content = Vec::new();
    if !text.trim().is_empty() {
        content.push(MessageContent::Text(text.trim().to_string()));
    }
    if let Some(tool_calls) = &choice.message.tool_calls {
        for tc in tool_calls {
            let arguments =
                serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| json!({}));
            content.push(MessageContent::ToolCall(crate::tool::ToolCall {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                arguments,
            }));
        }
    }

    let usage = parse_usage(&response.usage);
    let raw = serde_json::to_value(&response).unwrap_or_default();
    let id = response.id;
    let model = response.model;

    Ok(ProviderResponse {
        id: Some(id),
        model: Some(model),
        assistant_message: ChatMessage {
            role: MessageRole::Assistant,
            content,
        },
        reasoning_content,
        stop_reason: Some(map_stop_reason(&choice.finish_reason)),
        usage: Some(usage),
        raw: Some(raw),
    })
}

fn parse_usage(usage: &tiangong_deepseek::types::Usage) -> TokenUsageData {
    TokenUsageData {
        prompt_tokens: usage.prompt_tokens as usize,
        completion_tokens: usage.completion_tokens as usize,
        total_tokens: usage.total_tokens as usize,
        prompt_cache_hit_tokens: usage.prompt_cache_hit_tokens.map(|v| v as usize),
        prompt_cache_miss_tokens: usage.prompt_cache_miss_tokens.map(|v| v as usize),
    }
}

pub fn parse_stream_usage(usage: &tiangong_deepseek::types::Usage) -> TokenUsageData {
    parse_usage(usage)
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "tool_calls" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "content_filter" | "insufficient_system_resource" => StopReason::Other(reason.to_string()),
        _ => StopReason::Other(reason.to_string()),
    }
}
