use std::collections::BTreeMap;

use serde_json::Value;
use tiangong_anthropic::types::{
    ContentBlock, ContentBlockDeltaData, ContentBlockParam, ContentBlockStartData,
    Message as AnthropicMessage, MessageRole as AnthropicMessageRole, MessagesCreateRequest,
    MessagesCreateResponse, StreamEvent, ThinkingConfig, Tool as AnthropicTool,
    ToolChoice as AnthropicToolChoice, Usage,
};

use crate::error::LlmError;
use crate::message::{ChatMessage, MessageContent, MessageRole};
use crate::request::{ProviderRequest, ThinkingConfig as ProviderThinkingConfig};
use crate::response::{ProviderResponse, StopReason};
use crate::stream::ProviderStreamEvent;
use crate::tool::{ToolCall, ToolChoice, ToolResult, ToolResultContent};
use crate::usage::TokenUsageData;

pub(super) fn to_anthropic_request(
    request: &ProviderRequest,
) -> Result<MessagesCreateRequest, LlmError> {
    let messages = request
        .messages
        .iter()
        .filter_map(map_message)
        .collect::<Result<Vec<_>, _>>()?;

    let tools = if request.tools.is_empty() {
        None
    } else {
        Some(
            request
                .tools
                .iter()
                .map(|tool| AnthropicTool {
                    name: tool.name.clone(),
                    description: Some(tool.description.clone()),
                    input_schema: tool.input_schema.clone(),
                })
                .collect(),
        )
    };

    let tool_choice = request.tool_choice.as_ref().map(|choice| match choice {
        ToolChoice::Auto => AnthropicToolChoice::Auto,
        ToolChoice::Any => AnthropicToolChoice::Any,
        ToolChoice::Tool(name) => AnthropicToolChoice::Tool {
            name: name.clone(),
            disable_parallel_tool_use: None,
        },
    });

    Ok(MessagesCreateRequest {
        model: request.model.clone(),
        max_tokens: request.max_tokens.unwrap_or(4096),
        system: request
            .system
            .clone()
            .filter(|value| !value.trim().is_empty()),
        messages,
        temperature: request.temperature,
        stop_sequences: (!request.stop_sequences.is_empty())
            .then(|| request.stop_sequences.clone()),
        top_p: request.top_p,
        metadata: request.metadata.clone(),
        tools,
        tool_choice,
        stream: None,
        thinking: map_thinking_config(request.thinking.as_ref()),
    })
}

fn map_thinking_config(thinking: Option<&ProviderThinkingConfig>) -> Option<ThinkingConfig> {
    thinking.map(|thinking| ThinkingConfig::Enabled {
        budget_tokens: thinking.budget_tokens,
    })
}

fn map_message(message: &ChatMessage) -> Option<Result<AnthropicMessage, LlmError>> {
    match message.role {
        MessageRole::System => None,
        MessageRole::User | MessageRole::Assistant | MessageRole::Tool => {
            let role = match message.role {
                MessageRole::Assistant => AnthropicMessageRole::Assistant,
                _ => AnthropicMessageRole::User,
            };

            let content = message
                .content
                .iter()
                .map(map_content)
                .collect::<Result<Vec<_>, _>>();
            Some(content.map(|content| AnthropicMessage { role, content }))
        }
    }
}

fn map_content(content: &MessageContent) -> Result<ContentBlockParam, LlmError> {
    match content {
        MessageContent::Text(text) => Ok(ContentBlockParam::Text { text: text.clone() }),
        MessageContent::ToolCall(tool_call) => Ok(ContentBlockParam::ToolUse {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            input: tool_call.arguments.clone(),
        }),
        MessageContent::ToolResult(tool_result) => Ok(ContentBlockParam::ToolResult {
            tool_use_id: tool_result.tool_call_id.clone(),
            content: Some(match &tool_result.content {
                ToolResultContent::Text(text) => Value::String(text.clone()),
                ToolResultContent::Json(value) => value.clone(),
            }),
            is_error: Some(tool_result.is_error),
        }),
        MessageContent::Image(_) => Err(LlmError::UnsupportedFeature(
            "Anthropic 图片输入映射暂未实现".to_string(),
        )),
    }
}

pub(super) fn from_anthropic_response(
    response: MessagesCreateResponse,
) -> Result<ProviderResponse, LlmError> {
    let raw = serde_json::to_value(&response)
        .map(Some)
        .map_err(|err| LlmError::Serialization(err.to_string()))?;

    let mut reasoning_chunks = Vec::new();
    let assistant_content = response
        .content
        .into_iter()
        .filter_map(
            |content| match from_content(content, &mut reasoning_chunks) {
                Ok(Some(item)) => Some(Ok(item)),
                Ok(None) => None,
                Err(err) => Some(Err(err)),
            },
        )
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProviderResponse {
        id: Some(response.id),
        model: Some(response.model),
        assistant_message: ChatMessage {
            role: MessageRole::Assistant,
            content: assistant_content,
        },
        reasoning_content: (!reasoning_chunks.is_empty()).then(|| reasoning_chunks.join("")),
        stop_reason: response.stop_reason.as_deref().map(map_stop_reason),
        usage: response.usage.map(map_usage),
        raw,
    })
}

fn from_content(
    content: ContentBlock,
    reasoning_chunks: &mut Vec<String>,
) -> Result<Option<MessageContent>, LlmError> {
    match content {
        ContentBlock::Text { text } => Ok(Some(MessageContent::Text(text))),
        ContentBlock::ToolUse { id, name, input } => Ok(Some(MessageContent::ToolCall(ToolCall {
            id,
            name,
            arguments: input,
        }))),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => Ok(Some(MessageContent::ToolResult(ToolResult {
            tool_call_id: tool_use_id,
            content: match content {
                Some(Value::String(text)) => ToolResultContent::Text(text),
                Some(value) => ToolResultContent::Json(value),
                None => ToolResultContent::Text(String::new()),
            },
            is_error: is_error.unwrap_or(false),
        }))),
        ContentBlock::Thinking { thinking, .. } => {
            reasoning_chunks.push(thinking);
            Ok(None)
        }
        ContentBlock::RedactedThinking { .. } => Ok(None),
        ContentBlock::Unknown => Ok(None),
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Other(other.to_string()),
    }
}

fn map_usage(usage: Usage) -> TokenUsageData {
    TokenUsageData::new(
        usage.input_tokens.unwrap_or(0) as usize,
        usage.output_tokens.unwrap_or(0) as usize,
    )
}

#[derive(Default)]
pub(super) struct AnthropicStreamState {
    tool_calls: BTreeMap<usize, ToolCallAccumulator>,
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
}

pub(super) fn map_stream_event(
    state: &mut AnthropicStreamState,
    event: StreamEvent,
) -> Result<Vec<ProviderStreamEvent>, LlmError> {
    match event {
        StreamEvent::MessageStart { message } => {
            let mut events = vec![ProviderStreamEvent::MessageStart];
            if let Some(usage) = message.usage {
                events.push(ProviderStreamEvent::Usage(map_usage(usage)));
            }
            Ok(events)
        }
        StreamEvent::ContentBlockStart {
            index,
            content_block,
        } => match content_block {
            ContentBlockStartData::ToolUse { id, name, input } => {
                state
                    .tool_calls
                    .insert(index, ToolCallAccumulator { id: id.clone() });
                Ok(vec![ProviderStreamEvent::ToolCallStart(ToolCall {
                    id,
                    name,
                    arguments: input,
                })])
            }
            ContentBlockStartData::Thinking { thinking, .. } if !thinking.is_empty() => {
                Ok(vec![ProviderStreamEvent::ReasoningDelta(thinking)])
            }
            ContentBlockStartData::Text { text } if !text.is_empty() => {
                Ok(vec![ProviderStreamEvent::TextDelta(text)])
            }
            _ => Ok(Vec::new()),
        },
        StreamEvent::ContentBlockDelta { index, delta } => match delta {
            ContentBlockDeltaData::TextDelta { text } => {
                Ok(vec![ProviderStreamEvent::TextDelta(text)])
            }
            ContentBlockDeltaData::InputJsonDelta { partial_json } => {
                let call_id = state
                    .tool_calls
                    .get(&index)
                    .map(|call| call.id.clone())
                    .unwrap_or_else(|| format!("tool_call_{index}"));
                Ok(vec![ProviderStreamEvent::ToolCallDelta {
                    call_id,
                    partial_json,
                }])
            }
            ContentBlockDeltaData::ThinkingDelta { thinking } => {
                Ok(vec![ProviderStreamEvent::ReasoningDelta(thinking)])
            }
            ContentBlockDeltaData::SignatureDelta { .. } | ContentBlockDeltaData::Unknown => {
                Ok(Vec::new())
            }
        },
        StreamEvent::ContentBlockStop { index } => {
            if let Some(call) = state.tool_calls.remove(&index) {
                Ok(vec![ProviderStreamEvent::ToolCallEnd { call_id: call.id }])
            } else {
                Ok(Vec::new())
            }
        }
        StreamEvent::MessageDelta { usage, .. } => {
            if let Some(usage) = usage {
                Ok(vec![ProviderStreamEvent::Usage(map_usage(usage))])
            } else {
                Ok(Vec::new())
            }
        }
        StreamEvent::MessageStop => Ok(vec![ProviderStreamEvent::MessageEnd]),
        StreamEvent::Error { message } => Ok(vec![ProviderStreamEvent::Error(message)]),
        StreamEvent::Ping | StreamEvent::Unknown => Ok(Vec::new()),
    }
}

pub(super) fn map_stream_error(
    error: crate::error::LlmError,
) -> Vec<Result<ProviderStreamEvent, LlmError>> {
    vec![Err(error)]
}
