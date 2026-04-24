use anyhow::{Context, Result, anyhow};
use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    CreateChatCompletionRequestArgs,
};
use serde_json::{Value, json};

use crate::message::{ChatMessage, MessageContent, MessageRole};
use crate::request::ProviderRequest;
use crate::response::{ProviderResponse, StopReason};
use crate::tool::{ToolChoice, ToolSpec};
use crate::usage::TokenUsageData;

pub fn normalize_api_base(base_url: &str) -> Result<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("API_BASE_URL 不能为空"));
    }
    let cleaned = trimmed.trim_end_matches('/');
    let cleaned = cleaned.strip_suffix("/chat/completions").unwrap_or(cleaned);
    Ok(cleaned.to_string())
}

pub fn build_request_json(req: &ProviderRequest, stream: bool) -> Result<Value> {
    let messages = build_openai_messages(req)?;
    let mut request_args_binding = CreateChatCompletionRequestArgs::default();
    let mut request_args = request_args_binding
        .model(req.model.clone())
        .messages(messages)
        .stream(stream);
    if let Some(max_tokens) = req.max_tokens.map(|value| value as u16) {
        request_args = request_args.max_tokens(max_tokens);
    }
    if let Some(temperature) = req.temperature {
        request_args = request_args.temperature(temperature);
    }
    let request = request_args.build().context("构建 OpenAI 请求失败")?;

    let mut payload = serde_json::to_value(request).context("序列化 OpenAI 请求失败")?;
    if stream {
        inject_stream_usage_option(&mut payload);
    }
    if let Some(temperature) = req.temperature {
        inject_temperature_config(&mut payload, temperature)?;
    }
    if !req.tools.is_empty() {
        inject_function_tools(&mut payload, &req.tools, req.tool_choice.as_ref());
    }
    Ok(payload)
}

fn build_openai_messages(req: &ProviderRequest) -> Result<Vec<ChatCompletionRequestMessage>> {
    let mut messages = Vec::new();
    if let Some(system) = req.system.as_ref().filter(|value| !value.trim().is_empty()) {
        messages.push(
            ChatCompletionRequestSystemMessageArgs::default()
                .content(system.clone())
                .build()
                .context("构建 system 消息失败")?
                .into(),
        );
    }

    for message in &req.messages {
        match message.role {
            MessageRole::System => {}
            MessageRole::User | MessageRole::Tool => {
                let text = extract_message_text(message);
                if !text.is_empty() {
                    messages.push(
                        ChatCompletionRequestUserMessageArgs::default()
                            .content(text)
                            .build()
                            .context("构建 user 消息失败")?
                            .into(),
                    );
                }
            }
            MessageRole::Assistant => {
                let text = extract_message_text(message);
                if !text.is_empty() {
                    messages.push(
                        ChatCompletionRequestAssistantMessageArgs::default()
                            .content(text)
                            .build()
                            .context("构建 assistant 消息失败")?
                            .into(),
                    );
                }
            }
        }
    }

    Ok(messages)
}

fn extract_message_text(message: &ChatMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Text(text) => Some(text.clone()),
            MessageContent::ToolResult(result) => Some(match &result.content {
                crate::tool::ToolResultContent::Text(text) => text.clone(),
                crate::tool::ToolResultContent::Json(value) => value.to_string(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn parse_complete_response(payload: &Value) -> Result<ProviderResponse> {
    let choice = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| anyhow!("OpenAI 响应缺少 choices"))?;

    let text = choice
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let reasoning_content = choice
        .get("message")
        .and_then(|message| message.get("reasoning_content"))
        .and_then(Value::as_str)
        .map(|value| value.to_string());

    let tool_calls = choice
        .get("message")
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut content = Vec::new();
    if !text.trim().is_empty() {
        content.push(MessageContent::Text(strip_think_tags(text.trim())));
    }
    for tool_call in tool_calls {
        let id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name = tool_call
            .get("function")
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = tool_call
            .get("function")
            .and_then(|v| v.get("arguments"))
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_else(|| json!({}));
        if !name.is_empty() {
            content.push(MessageContent::ToolCall(crate::tool::ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments,
            }));
        }
    }

    let usage = payload.get("usage").map(parse_usage);

    Ok(ProviderResponse {
        id: payload
            .get("id")
            .and_then(Value::as_str)
            .map(|v| v.to_string()),
        model: payload
            .get("model")
            .and_then(Value::as_str)
            .map(|v| v.to_string()),
        assistant_message: ChatMessage {
            role: MessageRole::Assistant,
            content,
        },
        reasoning_content,
        stop_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(map_stop_reason),
        usage,
        raw: Some(payload.clone()),
    })
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "tool_calls" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        other => StopReason::Other(other.to_string()),
    }
}

pub fn parse_usage(usage: &Value) -> TokenUsageData {
    let prompt = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let completion = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or((prompt + completion) as u64) as usize;
    TokenUsageData {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
    }
}

fn inject_stream_usage_option(payload: &mut Value) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("stream_options".to_string(), json!({"include_usage": true}));
}

fn inject_temperature_config(payload: &mut Value, temperature: f32) -> Result<()> {
    let Some(obj) = payload.as_object_mut() else {
        return Ok(());
    };
    let number = serde_json::Number::from_f64(temperature as f64)
        .ok_or_else(|| anyhow!("temperature 无效"))?;
    obj.insert("temperature".to_string(), Value::Number(number));
    Ok(())
}

fn inject_function_tools(
    payload: &mut Value,
    functions: &[ToolSpec],
    tool_choice: Option<&ToolChoice>,
) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    let tools = functions
        .iter()
        .map(|spec| {
            json!({
                "type": "function",
                "function": {
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": spec.input_schema,
                }
            })
        })
        .collect::<Vec<_>>();
    obj.insert("tools".to_string(), Value::Array(tools));
    let tool_choice = match tool_choice {
        Some(ToolChoice::Any) => Value::String("required".to_string()),
        Some(ToolChoice::Tool(name)) => json!({
            "type": "function",
            "function": { "name": name },
        }),
        Some(ToolChoice::Auto) | None => Value::String("auto".to_string()),
    };
    obj.insert("tool_choice".to_string(), tool_choice);
}

pub fn strip_think_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find("<think>") {
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start..].find("</think>") {
            remaining = &remaining[start + end + 8..];
        } else {
            return result;
        }
    }
    result.push_str(remaining);
    result
}
