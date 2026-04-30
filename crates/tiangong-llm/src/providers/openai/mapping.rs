use anyhow::{Context, Result, anyhow};
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestToolMessage,
    ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs, FunctionCall,
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
            MessageRole::User => {
                if let Some(message) = build_openai_user_message(message)? {
                    messages.push(message);
                }
            }
            MessageRole::Tool => {
                for result in message.content.iter().filter_map(|content| match content {
                    MessageContent::ToolResult(result) => Some(result),
                    _ => None,
                }) {
                    let content = match &result.content {
                        crate::tool::ToolResultContent::Text(text) => text.clone(),
                        crate::tool::ToolResultContent::Json(value) => value.to_string(),
                    };
                    messages.push(ChatCompletionRequestMessage::Tool(
                        ChatCompletionRequestToolMessage {
                            content: content.into(),
                            tool_call_id: result.tool_call_id.clone(),
                        },
                    ));
                }
            }
            MessageRole::Assistant => {
                let text = extract_message_text(message);
                let tool_calls = message
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        MessageContent::ToolCall(tool_call) => {
                            Some(ChatCompletionMessageToolCalls::Function(
                                ChatCompletionMessageToolCall {
                                    id: tool_call.id.clone(),
                                    function: FunctionCall {
                                        name: tool_call.name.clone(),
                                        arguments: tool_call.arguments.to_string(),
                                    },
                                },
                            ))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !text.is_empty() || !tool_calls.is_empty() {
                    let mut args = ChatCompletionRequestAssistantMessageArgs::default();
                    if !text.is_empty() {
                        args.content(text);
                    }
                    if !tool_calls.is_empty() {
                        args.tool_calls(tool_calls);
                    }
                    messages.push(args.build().context("构建 assistant 消息失败")?.into());
                }
            }
        }
    }

    Ok(messages)
}

fn build_openai_user_message(
    message: &ChatMessage,
) -> Result<Option<ChatCompletionRequestMessage>> {
    let images = message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Image(image) => Some(image),
            _ => None,
        })
        .collect::<Vec<_>>();
    let text = extract_message_text(message);
    if images.is_empty() {
        if text.is_empty() {
            return Ok(None);
        }
        return Ok(Some(
            ChatCompletionRequestUserMessageArgs::default()
                .content(text)
                .build()
                .context("构建 user 消息失败")?
                .into(),
        ));
    }

    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    for image in images {
        content.push(json!({
            "type": "image_url",
            "image_url": { "url": image.data }
        }));
    }
    Ok(Some(
        serde_json::from_value(json!({
            "role": "user",
            "content": content
        }))
        .context("构建 OpenAI 多模态 user 消息失败")?,
    ))
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
    match tool_choice {
        Some(ToolChoice::Any) => {
            obj.insert(
                "tool_choice".to_string(),
                Value::String("required".to_string()),
            );
        }
        Some(ToolChoice::Tool(name)) => {
            obj.insert(
                "tool_choice".to_string(),
                json!({
                    "type": "function",
                    "function": { "name": name },
                }),
            );
        }
        // 兼容 vLLM / 部分 OpenAI-compatible 后端：
        // 显式 `tool_choice: "auto"` 可能要求服务端开启
        // --enable-auto-tool-choice 和 --tool-call-parser。
        // 省略该字段时，OpenAI Chat Completions 语义仍会在提供 tools 后
        // 使用默认自动选择策略，且不会触发这些后端的 400。
        Some(ToolChoice::Auto) => {}
        None => {}
    }
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
