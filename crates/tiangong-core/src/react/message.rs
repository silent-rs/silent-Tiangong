//! ReAct 循环中的消息构造、格式化和工具结果处理

use crate::session::{Message, MessageRole, MessageToolCall, Session};
use tiangong_types::{ContentBlock, StreamEvent, content_blocks_text, stable_content_blocks};

const TOOL_RESULT_STREAM_MAX_CHARS: usize = 8_000;

pub(crate) fn emit_session_message_upsert(
    ctx: &crate::turn_context::TurnContext,
    message_id: &str,
) {
    emit_session_message_upsert_with_state(ctx, message_id, false);
}

pub(crate) fn emit_session_message_upsert_with_state(
    ctx: &crate::turn_context::TurnContext,
    message_id: &str,
    include_deferred_tool_injections: bool,
) {
    let Some(mut message) = ctx
        .session
        .messages
        .iter()
        .find(|message| message.id == message_id)
        .cloned()
    else {
        return;
    };
    message.clear_transient_data();
    let _ = ctx.stream_tx.send(StreamEvent::SessionMessageUpsert {
        message,
        deferred_tool_injections: include_deferred_tool_injections
            .then(|| ctx.session.deferred_tool_injections.clone()),
    });
}

pub(crate) fn emit_deferred_tool_injections_changed(ctx: &crate::turn_context::TurnContext) {
    let _ = ctx
        .stream_tx
        .send(StreamEvent::DeferredToolInjectionsChanged {
            injections: ctx.session.deferred_tool_injections.clone(),
        });
}

pub(crate) fn defer_tool_injection(
    ctx: &mut crate::turn_context::TurnContext,
    tool_name: String,
    payload: serde_json::Value,
) {
    ctx.session.defer_tool_injection(tool_name, payload);
    ctx.session.persist_to_disk();
    emit_deferred_tool_injections_changed(ctx);
}

pub(crate) fn flush_deferred_tool_injections(ctx: &mut crate::turn_context::TurnContext) {
    if ctx.session.has_unfinished_tool_calls() {
        return;
    }
    for injection in std::mem::take(&mut ctx.session.deferred_tool_injections) {
        inject_tool_to_session(ctx, &injection.tool_name, &injection.payload);
    }
    ctx.session.persist_to_disk();
    emit_deferred_tool_injections_changed(ctx);
}

/// 持久化执行中的追加消息，并返回给当前 Agent 的纯文本输入。
pub(crate) fn accept_runtime_user_message(
    ctx: &mut crate::turn_context::TurnContext,
    message_id: Option<String>,
    prepared: Vec<ContentBlock>,
) -> Result<String, String> {
    let text = content_blocks_text(&prepared);
    let content_blocks = stable_content_blocks(&prepared);
    let message_id = message_id.unwrap_or_else(scru128::new_string);
    ctx.session
        .try_append_prepared_user_message_with_id(message_id.clone(), prepared)?;
    let _ = ctx.stream_tx.send(StreamEvent::UserMessage {
        message_id,
        content: text.clone(),
        content_blocks,
        media: Vec::new(),
        model_excluded: false,
    });
    Ok(text)
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
    )
    .with_phase(crate::session::MessagePhase::React);
    message.id = message_id;
    message.reasoning_signature = reasoning_signature;
    message.tool_calls = tool_calls;
    if let Some(existing) = session
        .messages
        .iter_mut()
        .find(|existing| existing.id == message.id && existing.role == MessageRole::Assistant)
    {
        *existing = message;
    } else {
        session.messages.push(message);
    }
}

pub(crate) fn upsert_assistant_text_message(
    session: &mut Session,
    message_id: &str,
    text: &str,
    reasoning_content: &str,
    phase: crate::session::MessagePhase,
) {
    let mut message = Message::with_reasoning(
        MessageRole::Assistant,
        text.to_string(),
        reasoning_content.to_string(),
    )
    .with_phase(phase);
    message.id = message_id.to_string();
    if let Some(existing) = session
        .messages
        .iter_mut()
        .find(|existing| existing.id == message_id && existing.role == MessageRole::Assistant)
    {
        *existing = message;
    } else {
        session.messages.push(message);
    }
}

pub(crate) fn append_tool_result_message(
    session: &mut Session,
    tool_call_id: &str,
    tool_name: &str,
    text: String,
    is_error: bool,
) {
    let message = Message::tool_result(tool_call_id, tool_name, text, is_error)
        .with_phase(crate::session::MessagePhase::React);
    session.messages.push(message);
}

pub fn append_runtime_tool_message(_session: &mut Session, tool_name: &str, content: String) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolFailureKind {
    Argument,
    PermissionDenied,
    UserRejected,
    CommandFailed,
    Timeout,
    EnvironmentMissing,
    Network,
    ToolInternal,
}

impl ToolFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Argument => "argument_error",
            Self::PermissionDenied => "permission_denied",
            Self::UserRejected => "user_rejected",
            Self::CommandFailed => "command_failed",
            Self::Timeout => "timeout",
            Self::EnvironmentMissing => "environment_missing",
            Self::Network => "network_failure",
            Self::ToolInternal => "tool_internal_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolFailureRecord {
    pub tool_name: String,
    pub tool_call_id: String,
    pub arguments_summary: String,
    pub error_kind: ToolFailureKind,
    pub error_message: String,
    pub retryable: bool,
    pub same_failure_count: usize,
    pub recommended_next_action: String,
    pub requires_user_input: bool,
}

impl ToolFailureRecord {
    pub(crate) fn new(
        tool_name: &str,
        tool_call_id: &str,
        arguments_summary: impl Into<String>,
        error_kind: ToolFailureKind,
        error_message: impl Into<String>,
    ) -> Self {
        let error_message = error_message.into();
        let retryable = default_retryable(error_kind);
        let requires_user_input = default_requires_user_input(error_kind);
        let recommended_next_action =
            default_recommended_next_action(error_kind, &error_message).to_string();
        Self {
            tool_name: tool_name.to_string(),
            tool_call_id: tool_call_id.to_string(),
            arguments_summary: arguments_summary.into(),
            error_kind,
            error_message,
            retryable,
            same_failure_count: 1,
            recommended_next_action,
            requires_user_input,
        }
    }

    pub(crate) fn repeated(
        tool_name: &str,
        tool_call_id: &str,
        arguments_summary: impl Into<String>,
        original_error: impl Into<String>,
    ) -> Self {
        let original_error = original_error.into();
        let mut record = Self::new(
            tool_name,
            tool_call_id,
            arguments_summary,
            classify_failure_message(&original_error),
            "",
        );
        record.error_message = original_error;
        record.retryable = false;
        record.same_failure_count = 2;
        record.requires_user_input = false;
        record.recommended_next_action =
            "不要重复相同工具和参数；请修正参数、切换工具，或在缺少外部条件时询问用户。"
                .to_string();
        record
    }

    pub(crate) fn render_for_model(&self) -> String {
        let arguments_summary = if self.arguments_summary.trim().is_empty() {
            "(empty)".to_string()
        } else {
            self.arguments_summary.trim().to_string()
        };
        format!(
            "[tool_failure]\n\
tool_name: {tool_name}\n\
tool_call_id: {tool_call_id}\n\
arguments_summary: {arguments_summary}\n\
error_kind: {error_kind}\n\
error_message: {error_message}\n\
retryable: {retryable}\n\
same_failure_count: {same_failure_count}\n\
requires_user_input: {requires_user_input}\n\
recommended_next_action: {recommended_next_action}",
            tool_name = self.tool_name,
            tool_call_id = self.tool_call_id,
            error_kind = self.error_kind.as_str(),
            error_message = self.error_message.trim(),
            retryable = self.retryable,
            same_failure_count = self.same_failure_count,
            requires_user_input = self.requires_user_input,
            recommended_next_action = self.recommended_next_action
        )
    }
}

pub(crate) fn classify_tool_result_failure(result: &crate::tool::ToolResult) -> ToolFailureKind {
    let combined = format!("{}\n{}", result.summary, result.stderr).to_lowercase();
    if combined.contains("timed out") || combined.contains("timeout") || combined.contains("超时")
    {
        ToolFailureKind::Timeout
    } else if combined.contains("not found")
        || combined.contains("no such file")
        || combined.contains("command not found")
        || combined.contains("未找到")
        || combined.contains("不存在")
        || combined.contains("缺失")
    {
        ToolFailureKind::EnvironmentMissing
    } else if combined.contains("network")
        || combined.contains("connection")
        || combined.contains("dns")
        || combined.contains("网络")
        || combined.contains("连接")
    {
        ToolFailureKind::Network
    } else if result.exit_code != 0 || !result.stderr.trim().is_empty() {
        ToolFailureKind::CommandFailed
    } else {
        ToolFailureKind::ToolInternal
    }
}

fn classify_failure_message(message: &str) -> ToolFailureKind {
    let lowered = message.to_lowercase();
    if lowered.contains("__parse_error") || lowered.contains("参数") || lowered.contains("json") {
        ToolFailureKind::Argument
    } else if lowered.contains("权限") || lowered.contains("permission") {
        ToolFailureKind::PermissionDenied
    } else if lowered.contains("拒绝") || lowered.contains("rejected") {
        ToolFailureKind::UserRejected
    } else if lowered.contains("timeout") || lowered.contains("超时") {
        ToolFailureKind::Timeout
    } else if lowered.contains("network") || lowered.contains("网络") {
        ToolFailureKind::Network
    } else {
        ToolFailureKind::ToolInternal
    }
}

fn default_retryable(kind: ToolFailureKind) -> bool {
    matches!(
        kind,
        ToolFailureKind::Timeout | ToolFailureKind::Network | ToolFailureKind::ToolInternal
    )
}

fn default_requires_user_input(kind: ToolFailureKind) -> bool {
    matches!(
        kind,
        ToolFailureKind::PermissionDenied | ToolFailureKind::UserRejected
    )
}

fn default_recommended_next_action(kind: ToolFailureKind, message: &str) -> &'static str {
    match kind {
        ToolFailureKind::Argument => {
            if message.contains("__parse_error") {
                "重新生成完整 JSON 参数，不要把 __parse_error 当作真实参数。"
            } else {
                "检查工具 schema 和参数类型，修正参数后再调用。"
            }
        }
        ToolFailureKind::PermissionDenied => {
            "不要重复执行被拒绝的操作；改用安全方案或请求用户授权。"
        }
        ToolFailureKind::UserRejected => {
            "用户已拒绝该操作；不要重复请求同一操作，改用不需要该授权的方案。"
        }
        ToolFailureKind::CommandFailed => "阅读 stderr/stdout，修正命令、路径或环境后再试。",
        ToolFailureKind::Timeout => "缩小操作范围、增加过滤条件，或改用更轻量的命令/工具。",
        ToolFailureKind::EnvironmentMissing => {
            "确认路径、命令、依赖或工作目录是否存在；缺少外部条件时询问用户。"
        }
        ToolFailureKind::Network => "检查网络、端点和凭据；可短暂重试，持续失败时询问用户。",
        ToolFailureKind::ToolInternal => "根据错误信息重新规划；不要盲目重复同一调用。",
    }
}

pub(crate) fn tool_result_provider_text(
    tool_name: &str,
    result: &crate::tool::ToolResult,
    _allow_memory_context: bool,
) -> String {
    // recall_memory 的引导文案已由 memory 插件内嵌进 ToolResult.stdout，
    // 不再需要 core 特判包装，统一走 tool_result_full_output。
    if is_media_tool_name(tool_name) && result.ok {
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
    ctx: &mut crate::turn_context::TurnContext,
    tool_call_id: &str,
    tool_name: &str,
) {
    let message = format!(
        "本轮已经成功执行过完全相同的 {tool_name} 工具调用，系统已跳过重复执行。\
        请查看上方历史消息中的工具执行结果，直接基于已有结果继续后续任务，不要再次发起相同调用。"
    );
    let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
        name: tool_name.to_string(),
        tool_call_id: Some(tool_call_id.to_string()),
        ok: true,
        output: message.clone(),
        full_output: Some(message.clone()),
        duration_ms: None,
    });
    append_tool_result_message(
        &mut ctx.session,
        tool_call_id,
        tool_name,
        message.clone(),
        false,
    );
    append_runtime_tool_message(
        &mut ctx.session,
        tool_name,
        format!("跳过重复工具调用 [{tool_name}]\n{message}"),
    );
}

pub(crate) fn append_repeated_failed_tool_result(
    ctx: &mut crate::turn_context::TurnContext,
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
        "{error_hint}本轮已经执行过完全相同的 {tool_name} 工具调用且执行失败，系统已跳过重复执行。请不要继续重复相同工具和参数；如果失败原因包含 __parse_error，请重新生成完整 JSON 参数，不要把 __parse_error 当作真实参数；也可以切换到其他可行方式。"
    );
    let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
        name: tool_name.to_string(),
        tool_call_id: Some(tool_call_id.to_string()),
        ok: false,
        output: message.clone(),
        full_output: Some(message.clone()),
        duration_ms: None,
    });
    append_tool_result_message(
        &mut ctx.session,
        tool_call_id,
        tool_name,
        message.clone(),
        true,
    );
    append_runtime_tool_message(
        &mut ctx.session,
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
pub fn inject_tool_to_session(
    ctx: &mut crate::turn_context::TurnContext,
    tool_name: &str,
    payload: &serde_json::Value,
) {
    // 先注入消息对（共用逻辑）
    let was_injected = inject_tool_to_messages(&mut ctx.session, tool_name, payload);
    if !was_injected {
        return;
    }
    // 找到刚注入的 tool result 消息，发送 StreamEvent
    let injected = ctx
        .session
        .messages
        .iter()
        .rev()
        .nth(1)
        .and_then(|assistant| {
            assistant.tool_calls.first().map(|tool_call| {
                let tool_result_id = ctx
                    .session
                    .messages
                    .last()
                    .map(|message| message.id.clone());
                let output = ctx
                    .session
                    .messages
                    .last()
                    .map(|message| message.text_content())
                    .unwrap_or_default();
                (
                    assistant.id.clone(),
                    tool_result_id,
                    tool_call.id.clone(),
                    output,
                )
            })
        });
    if let Some((assistant_id, tool_result_id, tool_call_id, output)) = injected {
        emit_session_message_upsert(ctx, &assistant_id);
        if let Some(tool_result_id) = tool_result_id {
            emit_session_message_upsert(ctx, &tool_result_id);
        }
        let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
            name: INJECTION_TOOL_NAME.to_string(),
            tool_call_id: Some(tool_call_id),
            ok: true,
            output,
            full_output: None,
            duration_ms: None,
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
    fn turn_finalization_closes_every_unfinished_tool_call_before_next_user() {
        let storage = tempfile::tempdir().unwrap();
        let mut session = Session::new("tool-interruption").with_storage_root(storage.path());
        let mut assistant = Message::new(MessageRole::Assistant, "");
        assistant.tool_calls = vec![
            MessageToolCall {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            },
            MessageToolCall {
                id: "call-2".to_string(),
                name: "write_file".to_string(),
                arguments: serde_json::json!({}),
            },
        ];
        session.messages.push(assistant);
        append_tool_result_message(
            &mut session,
            "call-1",
            "read_file",
            "done".to_string(),
            false,
        );

        let interrupted =
            session.close_unfinished_tool_calls_with_reason("工具调用因本轮结束而中断，未执行。");
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].0, "call-2");
        assert!(!session.has_unfinished_tool_calls());

        session.append_prepared_user_message_with_id(
            "next-user".to_string(),
            vec![ContentBlock::text("continue")],
        );
        let call_2_result = session
            .messages
            .iter()
            .position(|message| message.tool_call_id.as_deref() == Some("call-2"))
            .unwrap();
        let next_user = session
            .messages
            .iter()
            .position(|message| message.id == "next-user")
            .unwrap();
        assert!(call_2_result < next_user);
    }

    #[test]
    fn unfinished_tool_call_is_detected_before_context_injection() {
        let mut session = Session::new("deferred-injection");
        let mut assistant = Message::new(MessageRole::Assistant, "");
        assistant.tool_calls = vec![MessageToolCall {
            id: "call-pending".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({}),
        }];
        session.messages.push(assistant);

        assert!(session.has_unfinished_tool_calls());
        append_tool_result_message(
            &mut session,
            "call-pending",
            "write_file",
            "done".to_string(),
            false,
        );
        assert!(!session.has_unfinished_tool_calls());
    }

    #[test]
    fn reused_tool_call_id_does_not_reuse_an_old_result() {
        let storage = tempfile::tempdir().unwrap();
        let mut session = Session::new("reused-tool-id").with_storage_root(storage.path());
        for completed in [true, false] {
            let mut assistant = Message::new(MessageRole::Assistant, "");
            assistant.tool_calls = vec![MessageToolCall {
                id: "call-1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            }];
            session.messages.push(assistant);
            if completed {
                append_tool_result_message(
                    &mut session,
                    "call-1",
                    "read_file",
                    "old result".to_string(),
                    false,
                );
            }
        }

        assert!(session.has_unfinished_tool_calls());
        let interrupted =
            session.close_unfinished_tool_calls_with_reason("工具调用因本轮结束而中断，未执行。");
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].0, "call-1");
        assert!(!session.has_unfinished_tool_calls());
    }

    #[test]
    fn structured_tool_failure_renders_argument_guidance() {
        let record = ToolFailureRecord::new(
            "read_file",
            "call_bad",
            "path=(empty)",
            ToolFailureKind::Argument,
            "工具参数 JSON 无效：__parse_error",
        );

        let text = record.render_for_model();

        assert!(text.contains("[tool_failure]"));
        assert!(text.contains("tool_name: read_file"));
        assert!(text.contains("error_kind: argument_error"));
        assert!(text.contains("retryable: false"));
        assert!(text.contains("不要把 __parse_error 当作真实参数"));
    }

    #[test]
    fn classify_tool_result_failure_distinguishes_common_kinds() {
        let command_failed = crate::tool::ToolResult {
            ok: false,
            summary: "命令执行失败".to_string(),
            stdout: String::new(),
            stderr: "exit status 2".to_string(),
            exit_code: 2,
            execution: None,
        };
        assert_eq!(
            classify_tool_result_failure(&command_failed),
            ToolFailureKind::CommandFailed
        );

        let missing_environment = crate::tool::ToolResult {
            ok: false,
            summary: "工具执行失败".to_string(),
            stdout: String::new(),
            stderr: "command not found: rg".to_string(),
            exit_code: 127,
            execution: None,
        };
        assert_eq!(
            classify_tool_result_failure(&missing_environment),
            ToolFailureKind::EnvironmentMissing
        );
    }
}
