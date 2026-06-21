//! ReAct 循环中的消息构造、格式化和工具结果处理

use std::sync::mpsc::Sender as StdSender;

use crate::session::{Message, MessageRole, MessageToolCall, Session};
use tiangong_types::{MediaAsset, StreamEvent};

const MEMORY_LOOP_FEEDBACK_MAX_CHARS: usize = 12_000;
const TOOL_RESULT_STREAM_MAX_CHARS: usize = 8_000;

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
            let has_media = message.content.iter().any(|b| !b.is_text());
            if !has_media && !media.is_empty() {
                for asset in &media {
                    message.content.push(asset.to_content_block());
                }
                message.media_migrated = true;
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
}

pub(crate) fn append_runtime_tool_message(
    _session: &mut Session,
    tool_name: &str,
    content: String,
) {
    tracing::info!(tool_name, content, "runtime trace");
}

pub(crate) fn append_runtime_tool_message_with_reasoning(
    _session: &mut Session,
    tool_name: &str,
    content: String,
    reasoning_content: String,
) {
    tracing::info!(
        tool_name,
        content,
        reasoning_content,
        "runtime trace with reasoning"
    );
}

pub(crate) fn tool_result_provider_text(
    tool_name: &str,
    result: &crate::tool::ToolResult,
    allow_memory_context: bool,
) -> String {
    if tool_name == "recall_memory" {
        build_memory_recall_feedback(&result.stdout, allow_memory_context)
    } else if is_media_tool_name(tool_name) && result.ok {
        let media_desc = if result.stdout.trim().is_empty() {
            result.summary.clone()
        } else {
            format!("{}\n{}", result.summary, result.stdout)
        };
        format!(
            "工具 {tool_name} 执行成功：{}。不要再次调用该工具。",
            media_desc
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
        "本轮已经成功执行过完全相同的 {tool_name} 工具调用，系统已跳过重复执行。\
        请查看上方历史消息中的工具执行结果，直接基于已有结果继续后续任务，不要再次发起相同调用。"
    );
    let _ = stream_tx.send(StreamEvent::ToolResult {
        name: tool_name.to_string(),
        tool_call_id: Some(tool_call_id.to_string()),
        ok: true,
        output: message.clone(),
        full_output: Some(message.clone()),
        media: vec![],
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
    original_error: &str,
) {
    let error_hint = if original_error.trim().is_empty() {
        String::new()
    } else {
        format!("失败原因：{original_error}\n")
    };
    let message = format!(
        "{error_hint}本轮已经执行过完全相同的 {tool_name} 工具调用且执行失败，系统已跳过重复执行。请不要继续重复相同工具和参数；请修正参数后重试，或切换到其他可行方式。"
    );
    let _ = stream_tx.send(StreamEvent::ToolResult {
        name: tool_name.to_string(),
        tool_call_id: Some(tool_call_id.to_string()),
        ok: false,
        output: message.clone(),
        full_output: Some(message.clone()),
        media: vec![],
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

pub(crate) fn latest_user_message(session: &Session) -> String {
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.text_content())
        .unwrap_or_default()
}

/// 注入工具的 tool_call name（注册在 tool spec 中，声明 Agent 不调用）。
pub const INJECTION_TOOL_NAME: &str = "plugin_injection";

/// 向 session 注入工具消息（assistant tool_call + tool result 消息对）。
///
/// tool_call name 统一用 `plugin_injection`（注册的注入工具），原始来源 tool_name
/// 放入 payload 的 `source` 字段，让 Agent 知道数据来源。
/// 去重：与上一条 plugin_injection 消息渲染文本完全相同则跳过。
pub fn inject_tool_to_messages(
    session: &mut Session,
    tool_name: &str,
    payload: &serde_json::Value,
) -> bool {
    // 把来源 tool_name 注入 payload
    let mut full_payload = payload.clone();
    if let Some(obj) = full_payload.as_object_mut() {
        obj.insert(
            "source".to_string(),
            serde_json::Value::String(tool_name.to_string()),
        );
    }
    let output = render_tool_output(tool_name, payload);
    if output.trim().is_empty() {
        return false;
    }
    let is_dup = session
        .messages
        .iter()
        .rev()
        .find(|msg| {
            msg.role == MessageRole::Tool && msg.tool_name.as_deref() == Some(INJECTION_TOOL_NAME)
        })
        .is_some_and(|msg| msg.text_content() == output);
    if is_dup {
        tracing::debug!(session_id = %session.id, tool_name, "skip tool injection: identical to previous");
        return false;
    }
    let tool_call_id = format!("inj_{}", scru128::new());
    // assistant 消息只承载 tool_call，text 留空（前端不显示空 text 的 assistant 消息）
    let mut assistant_msg = Message::new(MessageRole::Assistant, String::new());
    assistant_msg.tool_calls = vec![MessageToolCall {
        id: tool_call_id.clone(),
        name: INJECTION_TOOL_NAME.to_string(),
        arguments: full_payload,
    }];
    session.messages.push(assistant_msg);
    append_tool_result_message(session, &tool_call_id, INJECTION_TOOL_NAME, output, false);
    tracing::info!(session_id = %session.id, tool_name, "tool content injected into session");
    true
}

/// 向 session 注入工具消息并发送 StreamEvent（core worker 路径使用）。
pub(crate) fn inject_tool_to_session(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    tool_name: &str,
    payload: &serde_json::Value,
) {
    // 先注入消息对（共用逻辑）
    let was_injected = inject_tool_to_messages(session, tool_name, payload);
    if !was_injected {
        return;
    }
    // 找到刚注入的 tool result 消息，发送 StreamEvent
    if let Some(assistant_msg) = session.messages.iter().rev().nth(1)
        && let Some(tc) = assistant_msg.tool_calls.first()
    {
        let output = session
            .messages
            .last()
            .map(|m| m.text_content())
            .unwrap_or_default();
        let _ = stream_tx.send(StreamEvent::ToolResult {
            name: INJECTION_TOOL_NAME.to_string(),
            tool_call_id: Some(tc.id.clone()),
            ok: true,
            output,
            full_output: None,
            media: vec![],
        });
    }
}

/// 渲染 JSON payload 为对话文本（通用格式）。
///
/// 格式：
/// ```text
/// 数据来源：browser_data
/// 相关数据：
///   title: API Keys
///   url: https://example.com
///   text: 页面文本内容...
/// ```
///
/// 递归展开嵌套对象和数组，适合任意插件注入。
pub fn render_tool_output(tool_name: &str, payload: &serde_json::Value) -> String {
    let mut output = format!("数据来源：{tool_name}");
    if let Some(obj) = payload.as_object()
        && !obj.is_empty()
    {
        output.push_str("\n相关数据：");
        for (key, value) in obj {
            output.push_str(&format!("\n    {key}: {}", format_payload_value(value)));
        }
    }
    output
}

/// 递归格式化 payload 值为可读文本。
fn format_payload_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                "[]".to_string()
            } else {
                let items: Vec<String> = arr
                    .iter()
                    .map(|item| format!("        - {}", format_payload_value(item)))
                    .collect();
                format!("\n{}", items.join("\n"))
            }
        }
        serde_json::Value::Object(obj) => {
            if obj.is_empty() {
                "{}".to_string()
            } else {
                let items: Vec<String> = obj
                    .iter()
                    .map(|(k, v)| format!("        {k}: {}", format_payload_value(v)))
                    .collect();
                format!("\n{}", items.join("\n"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_tool_renders_browser_feedback() {
        let mut session = Session::new("browser");
        let (tx, rx) = std::sync::mpsc::channel();

        inject_tool_to_session(
            &mut session,
            &tx,
            "browser_data",
            &serde_json::json!({
                "title": "API Keys",
                "url": "https://platform.deepseek.com/api_keys",
                "text": "API keys page",
                "feedback": "POST /api_keys (状态 200)\n{\"key\":\"sk-test\"}",
            }),
        );

        // 消息对：assistant(tool_call) + tool(result)
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].tool_calls.len(), 1);
        assert_eq!(
            session.messages[1].tool_call_id.as_deref(),
            Some(session.messages[0].tool_calls[0].id.as_str())
        );

        let tool_text = session.messages[1].text_content();
        assert!(tool_text.contains("数据来源：browser_data"));
        assert!(tool_text.contains("title: API Keys"));
        assert!(tool_text.contains("sk-test"));

        let event = rx.recv().unwrap();
        match event {
            StreamEvent::ToolResult { output, .. } => {
                assert!(output.contains("数据来源：browser_data"));
                assert!(output.contains("sk-test"));
            }
            _ => panic!("expected tool result event"),
        }
    }

    #[test]
    fn inject_tool_dedup_by_url() {
        let mut session = Session::new("browser");
        let (tx, _rx) = std::sync::mpsc::channel();
        let payload = serde_json::json!({
            "title": "API Keys",
            "url": "https://platform.deepseek.com/api_keys",
            "text": "API keys page",
        });

        inject_tool_to_session(&mut session, &tx, "browser_data", &payload);
        assert_eq!(session.messages.len(), 2);

        // 同 payload → 去重跳过
        inject_tool_to_session(&mut session, &tx, "browser_data", &payload);
        assert_eq!(session.messages.len(), 2);
    }

    #[test]
    fn inject_tool_renders_terminal_user_input() {
        let mut session = Session::new("terminal");
        let (tx, _rx) = std::sync::mpsc::channel();

        inject_tool_to_session(
            &mut session,
            &tx,
            "terminal_user_input",
            &serde_json::json!({ "command": "ls -la" }),
        );

        assert_eq!(session.messages.len(), 2);
        let tool_text = session.messages[1].text_content();
        assert!(tool_text.contains("数据来源：terminal_user_input"));
        assert!(tool_text.contains("command: ls -la"));
    }
}
