//! ReAct 循环中的消息构造、格式化和工具结果处理

use std::sync::mpsc::Sender as StdSender;

use crate::session::{Message, MessageRole, MessageToolCall, Session, now_text};
use tiangong_types::{MediaAsset, StreamEvent};

const MEMORY_LOOP_FEEDBACK_MAX_CHARS: usize = 12_000;
const TOOL_RESULT_STREAM_MAX_CHARS: usize = 8_000;

pub(crate) fn append_user_message_to_loop_context(
    session: &mut Session,
    loop_context: &mut Vec<Message>,
    stream_tx: &StdSender<StreamEvent>,
    content: String,
    message_id: Option<String>,
    media: Vec<MediaAsset>,
) {
    let loop_message_id = append_or_reuse_user_message(session, &content, message_id, media);
    let _ = stream_tx.send(StreamEvent::UserMessage {
        message_id: loop_message_id.clone(),
        content: content.clone(),
        media: session
            .messages
            .iter()
            .find(|message| message.id == loop_message_id)
            .map(|message| message.media.clone())
            .unwrap_or_default(),
    });
    if let Some(message) = session
        .messages
        .iter()
        .find(|message| message.id == loop_message_id)
    {
        loop_context.push(message.clone());
    }
}

pub(crate) fn append_or_reuse_user_message(
    session: &mut Session,
    content: &str,
    message_id: Option<String>,
    media: Vec<MediaAsset>,
) -> String {
    if let Some(message_id) = message_id {
        if let Some(message) = session
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
        {
            if message.media.is_empty() && !media.is_empty() {
                message.media = media;
            }
        } else {
            session.append_message_with_id_and_media(
                message_id.clone(),
                MessageRole::User,
                content.to_string(),
                String::new(),
                media,
            );
        }
        return message_id;
    }

    session.append_message_with_media(MessageRole::User, content.to_string(), media);
    session
        .messages
        .last()
        .map(|m| m.id.clone())
        .unwrap_or_default()
}

pub(crate) fn is_synthetic_tool_call_placeholder(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("[调用工具:") && trimmed.ends_with(']')
}

pub(crate) fn append_assistant_tool_call_message(
    session: &mut Session,
    message_id: String,
    text: &str,
    reasoning_content: &str,
    reasoning_signature: Option<String>,
    calls: &[&crate::model::ToolCall],
) {
    let tool_calls = calls
        .iter()
        .map(|call| MessageToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
        .collect::<Vec<_>>();
    if tool_calls.is_empty() {
        return;
    }

    let mut message = Message::with_reasoning(
        MessageRole::Assistant,
        text.trim().to_string(),
        reasoning_content.trim().to_string(),
    );
    message.id = message_id;
    message.reasoning_signature = reasoning_signature;
    message.tool_calls = tool_calls;
    session.messages.push(message);
    session.updated_at = now_text();
}

pub(crate) fn append_tool_result_message(
    session: &mut Session,
    tool_call_id: &str,
    tool_name: &str,
    text: String,
    is_error: bool,
) {
    let mut message = Message::new(MessageRole::Tool, text);
    message.tool_call_id = Some(tool_call_id.to_string());
    message.tool_name = Some(tool_name.to_string());
    message.tool_result_is_error = is_error;
    session.messages.push(message);
    session.updated_at = now_text();
}

pub(crate) fn append_runtime_tool_message(session: &mut Session, tool_name: &str, content: String) {
    let mut message = Message::new(MessageRole::Tool, content);
    message.tool_name = Some(tool_name.to_string());
    session.messages.push(message);
    session.updated_at = now_text();
}

pub(crate) fn append_runtime_tool_message_with_reasoning(
    session: &mut Session,
    tool_name: &str,
    content: String,
    reasoning_content: String,
) {
    let mut message = Message::with_reasoning(MessageRole::Tool, content, reasoning_content);
    message.tool_name = Some(tool_name.to_string());
    session.messages.push(message);
    session.updated_at = now_text();
}

pub(crate) fn tool_result_provider_text(
    tool_name: &str,
    result: &crate::tool::ToolResult,
    allow_memory_context: bool,
) -> String {
    if tool_name == "recall_memory" {
        build_memory_recall_feedback(&result.stdout, allow_memory_context)
    } else if is_media_tool_name(tool_name) && result.ok {
        format!(
            "工具 {tool_name} 执行成功：{}。媒体内容已生成并交付给用户，不要再次调用该工具。",
            result.summary
        )
    } else {
        tool_result_full_output(result)
    }
}

pub(crate) fn tool_call_dedupe_key(tool_name: &str, arguments: &serde_json::Value) -> String {
    format!("{tool_name}\n{}", canonical_json(arguments))
}

pub(crate) fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => {
            serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
        }
        serde_json::Value::Array(values) => {
            let items = values.iter().map(canonical_json).collect::<Vec<_>>();
            format!("[{}]", items.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            let items = entries
                .into_iter()
                .map(|(key, value)| {
                    let key = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string());
                    format!("{key}:{}", canonical_json(value))
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", items.join(","))
        }
    }
}

pub(crate) fn append_duplicate_tool_result(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    tool_call_id: &str,
    tool_name: &str,
) {
    let message = format!(
        "本轮已经成功执行过完全相同的 {tool_name} 工具调用，系统已跳过重复执行。请直接基于前一次工具结果继续完成用户目标，不要再次调用相同工具和参数。"
    );
    let _ = stream_tx.send(StreamEvent::ToolResult {
        name: tool_name.to_string(),
        tool_call_id: Some(tool_call_id.to_string()),
        ok: true,
        output: message.clone(),
        full_output: Some(message.clone()),
    });
    append_tool_result_message(session, tool_call_id, tool_name, message.clone(), false);
    append_runtime_tool_message(
        session,
        tool_name,
        format!("跳过重复工具调用 [{tool_name}]\n{message}"),
    );
}

pub(crate) fn append_repeated_failed_tool_result(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    tool_call_id: &str,
    tool_name: &str,
) {
    let message = format!(
        "本轮已经执行过完全相同的 {tool_name} 工具调用且执行失败，系统已跳过重复执行。请不要继续重复相同工具和参数；请切换到其他可行方式，或在缺少外部条件时请求用户协作。"
    );
    let _ = stream_tx.send(StreamEvent::ToolResult {
        name: tool_name.to_string(),
        tool_call_id: Some(tool_call_id.to_string()),
        ok: false,
        output: message.clone(),
        full_output: Some(message.clone()),
    });
    append_tool_result_message(session, tool_call_id, tool_name, message.clone(), true);
    append_runtime_tool_message(
        session,
        tool_name,
        format!("跳过重复失败工具调用 [{tool_name}]\n{message}"),
    );
}

pub(crate) fn is_media_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "generate_image" | "generate_video" | "text_to_speech" | "speech_to_text"
    )
}

pub(crate) fn build_memory_recall_feedback(stdout: &str, allow_memory_context: bool) -> String {
    let header = if allow_memory_context && !stdout.trim().is_empty() {
        "recall_memory 已完成。以下是可直接使用的回忆结果，请基于这些内容继续完成用户原始目标；不要再次调用 recall_memory，除非用户提出新的历史查询。"
    } else if stdout.trim().is_empty() {
        "recall_memory 已完成，但没有可用的增量历史记忆。请基于当前上下文继续完成用户原始目标；不要再次调用 recall_memory。"
    } else {
        "recall_memory 已完成，结果如下。请基于当前上下文继续完成用户原始目标；不要再次调用 recall_memory。"
    };
    let body = truncate_chars_with_notice(
        stdout.trim(),
        MEMORY_LOOP_FEEDBACK_MAX_CHARS,
        "\n...(已截断，完整回忆结果已记录在工具执行消息中)",
    );
    if body.trim().is_empty() {
        header.to_string()
    } else {
        format!("{header}\n\n{body}")
    }
}

pub(crate) fn tool_result_full_output(result: &crate::tool::ToolResult) -> String {
    if result.ok {
        return if result.stdout.trim().is_empty() {
            result.summary.clone()
        } else {
            result.stdout.clone()
        };
    }

    let mut lines = Vec::new();
    if !result.summary.trim().is_empty() {
        lines.push(format!("summary: {}", result.summary));
    }
    if !result.stderr.trim().is_empty() {
        lines.push(format!("stderr:\n{}", result.stderr));
    }
    if !result.stdout.trim().is_empty() {
        lines.push(format!("stdout:\n{}", result.stdout));
    }
    if lines.is_empty() {
        "工具执行失败，但没有返回详细错误".to_string()
    } else {
        lines.join("\n")
    }
}

pub(crate) fn tool_result_stream_output(result: &crate::tool::ToolResult) -> String {
    let output = tool_result_full_output(result);
    truncate_chars_with_notice(
        &output,
        TOOL_RESULT_STREAM_MAX_CHARS,
        "\n...(已截断，完整工具输出已记录到会话数据)",
    )
}

pub(crate) fn truncate_chars_with_notice(text: &str, max_chars: usize, notice: &str) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}{notice}")
    } else {
        truncated
    }
}

pub(crate) fn latest_user_message(session: &Session) -> &str {
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.content.as_str())
        .unwrap_or_default()
}
