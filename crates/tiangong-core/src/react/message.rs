//! ReAct 循环中的消息构造、格式化和工具结果处理

use std::sync::mpsc::Sender as StdSender;
use std::sync::{Arc, Mutex};

use crate::agent_team::lifecycle::TeamContext;
use crate::session::{Message, MessageRole, MessageToolCall, PendingAgentDelivery, Session};
use tiangong_types::{
    ContentBlock, StreamEvent, content_blocks_text, stable_content_blocks,
    validate_ready_content_blocks,
};

const TOOL_RESULT_STREAM_MAX_CHARS: usize = 8_000;

pub fn append_or_reuse_user_message(
    session: &mut Session,
    message_id: Option<String>,
    prepared: Vec<ContentBlock>,
) -> Result<String, String> {
    if let Some(message_id) = message_id {
        if session
            .messages
            .iter()
            .any(|message| message.id == message_id)
        {
            if !session.update_prepared_user_message(&message_id, prepared) {
                return Err(format!("消息 ID {message_id} 已被非用户消息占用"));
            }
        } else {
            session.append_prepared_user_message_with_id(message_id.clone(), prepared);
        }
        return Ok(message_id);
    }

    let message_id = scru128::new().to_string();
    session.append_prepared_user_message_with_id(message_id.clone(), prepared);
    Ok(message_id)
}

pub(crate) struct AcceptedUserMessage {
    pub message_id: String,
    /// 该用户消息在 Session 中的实际索引；快照预含同 ID 消息时可能小于接收前 len。
    pub turn_start_idx: usize,
    pub prepared: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeMessageDisposition {
    CurrentAgentInput(String),
    RoutedToAgent,
}

pub(crate) fn emit_session_message_upsert(
    session: &Session,
    stream_tx: &StdSender<StreamEvent>,
    message_id: &str,
) {
    emit_session_message_upsert_with_state(session, stream_tx, message_id, false, false);
}

pub(crate) fn emit_session_message_upsert_with_state(
    session: &Session,
    stream_tx: &StdSender<StreamEvent>,
    message_id: &str,
    include_pending_agent_deliveries: bool,
    include_deferred_tool_injections: bool,
) {
    let Some(mut message) = session
        .messages
        .iter()
        .find(|message| message.id == message_id)
        .cloned()
    else {
        return;
    };
    message.clear_transient_data();
    let _ = stream_tx.send(StreamEvent::SessionMessageUpsert {
        message,
        pending_agent_deliveries: include_pending_agent_deliveries
            .then(|| session.pending_agent_deliveries.clone()),
        deferred_tool_injections: include_deferred_tool_injections
            .then(|| session.deferred_tool_injections.clone()),
    });
}

pub(crate) fn emit_pending_agent_deliveries_changed(
    session: &Session,
    stream_tx: &StdSender<StreamEvent>,
) {
    let mut deliveries = session.pending_agent_deliveries.clone();
    for delivery in &mut deliveries {
        for block in &mut delivery.additional_content {
            block.clear_transient_data();
        }
    }
    let _ = stream_tx.send(StreamEvent::PendingAgentDeliveriesChanged { deliveries });
}

pub(crate) fn emit_deferred_tool_injections_changed(
    session: &Session,
    stream_tx: &StdSender<StreamEvent>,
) {
    let _ = stream_tx.send(StreamEvent::DeferredToolInjectionsChanged {
        injections: session.deferred_tool_injections.clone(),
    });
}

pub(crate) fn defer_tool_injection(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    tool_name: String,
    payload: serde_json::Value,
) {
    session.defer_tool_injection(tool_name, payload);
    session.persist_to_disk();
    emit_deferred_tool_injections_changed(session, stream_tx);
}

/// 把 Prepared 用户消息写入 Session，持久化成功后才确认并发出 UserMessage 事件。
///
/// 失败时恢复完整的投递前 Session 快照，保证消息和运行态内容都不会半写入。
#[allow(clippy::too_many_arguments)]
pub(crate) fn accept_prepared_user_message_with_options(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    message_id: Option<String>,
    prepared: Vec<ContentBlock>,
    persistence_ack: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    pending_agent_deliveries: Vec<PendingAgentDelivery>,
    model_excluded: bool,
    close_pending_tools: bool,
) -> Result<AcceptedUserMessage, String> {
    if let Err(err) = validate_ready_content_blocks(&prepared) {
        if let Some(ack) = persistence_ack {
            let _ = ack.send(Err(err.clone()));
        }
        return Err(err);
    }
    let before = session.clone();
    // Desktop/Server 会先把稳定 User 乐观写入宿主快照。执行中追加时必须先取出
    // 这条预写消息，闭合未完成 tool calls 后再把 User 放回末尾，保持协议顺序。
    if let Some(message_id) = message_id.as_deref()
        && let Some(index) = session
            .messages
            .iter()
            .position(|message| message.id == message_id && message.role == MessageRole::User)
    {
        session.messages.remove(index);
    }
    let interrupted_tool_calls = if close_pending_tools {
        close_unfinished_tool_calls(session)
    } else {
        Vec::new()
    };
    let text = content_blocks_text(&prepared);
    let accepted_prepared = prepared.clone();
    let stable_content = stable_content_blocks(&prepared);
    let message_id = match append_or_reuse_user_message(session, message_id, prepared) {
        Ok(message_id) => message_id,
        Err(err) => {
            *session = before;
            if let Some(ack) = persistence_ack {
                let _ = ack.send(Err(err.clone()));
            }
            return Err(err);
        }
    };
    let turn_start_idx = match user_message_turn_start_idx(session, &message_id) {
        Some(index) => index,
        None => {
            let err = format!("用户消息 {message_id} 写入后未出现在 Session 中");
            *session = before;
            if let Some(ack) = persistence_ack {
                let _ = ack.send(Err(err.clone()));
            }
            return Err(err);
        }
    };
    if let Some(message) = session.messages.get_mut(turn_start_idx) {
        message.model_excluded = model_excluded;
    }
    session.replace_pending_agent_deliveries(&message_id, pending_agent_deliveries);

    if let Err(err) = session.try_persist_to_disk() {
        *session = before;
        if let Some(ack) = persistence_ack {
            let _ = ack.send(Err(err.clone()));
        }
        return Err(err);
    }

    if let Some(ack) = persistence_ack {
        let _ = ack.send(Ok(()));
    }
    for (tool_call_id, tool_name, output) in interrupted_tool_calls {
        let _ = stream_tx.send(StreamEvent::ToolResult {
            name: tool_name,
            tool_call_id: Some(tool_call_id),
            ok: false,
            output,
            full_output: None,
            duration_ms: None,
        });
    }
    let media = session
        .messages
        .iter()
        .find(|message| message.id == message_id)
        .map(|message| message.extract_media_assets())
        .unwrap_or_default();
    let _ = stream_tx.send(StreamEvent::UserMessage {
        message_id: message_id.clone(),
        content: text.clone(),
        content_blocks: stable_content,
        media: media.clone(),
        model_excluded,
        pending_agent_deliveries: session.pending_agent_deliveries.clone(),
    });
    Ok(AcceptedUserMessage {
        message_id,
        turn_start_idx,
        prepared: accepted_prepared,
    })
}

/// 在追加新的 User 消息前闭合所有尚未返回结果的工具调用，保持 Provider 协议顺序。
fn close_unfinished_tool_calls(session: &mut Session) -> Vec<(String, String, String)> {
    close_unfinished_tool_calls_with_reason(session, "工具调用因新的用户消息而中断，未执行。")
}

/// 在轮次终态提交前闭合所有未完成的工具调用，保持 Provider 历史协议完整。
pub(crate) fn close_unfinished_tool_calls_for_turn(
    session: &mut Session,
) -> Vec<(String, String, String)> {
    close_unfinished_tool_calls_with_reason(session, "工具调用因本轮结束而中断，未执行。")
}

fn close_unfinished_tool_calls_with_reason(
    session: &mut Session,
    reason: &str,
) -> Vec<(String, String, String)> {
    let pending = unfinished_tool_calls(session);

    pending
        .into_iter()
        .map(|(tool_call_id, tool_name)| {
            let output = reason.to_string();
            append_tool_result_message(session, &tool_call_id, &tool_name, output.clone(), true);
            (tool_call_id, tool_name, output)
        })
        .collect()
}

pub(crate) fn has_unfinished_tool_calls(session: &Session) -> bool {
    !unfinished_tool_calls(session).is_empty()
}

pub(crate) fn flush_deferred_tool_injections(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
) {
    if has_unfinished_tool_calls(session) {
        return;
    }
    for injection in std::mem::take(&mut session.deferred_tool_injections) {
        inject_tool_to_session(session, stream_tx, &injection.tool_name, &injection.payload);
    }
    session.persist_to_disk();
    emit_deferred_tool_injections_changed(session, stream_tx);
}

fn unfinished_tool_calls(session: &Session) -> Vec<(String, String)> {
    let Some((assistant_index, assistant)) = session
        .messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, message)| !message.tool_calls.is_empty())
    else {
        return Vec::new();
    };
    let completed = session.messages[assistant_index + 1..]
        .iter()
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect::<std::collections::HashSet<_>>();
    assistant
        .tool_calls
        .iter()
        .filter(|call| !completed.contains(call.id.as_str()))
        .map(|call| (call.id.clone(), call.name.clone()))
        .collect()
}

/// 持久化执行中的追加消息，并在主 Agent 上统一处理 `@Agent` 定向投递。
///
/// 子 Agent 收到的内部 `Command::Message` 已经完成定向，不再重复解析提及。
#[allow(clippy::too_many_arguments)]
pub(crate) fn accept_runtime_user_message(
    agent_id: &str,
    team: Option<&Arc<Mutex<TeamContext>>>,
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    message_id: Option<String>,
    prepared: Vec<ContentBlock>,
    persistence_ack: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
) -> Result<RuntimeMessageDisposition, String> {
    // 路由预判和实际投递使用同一把团队锁，避免两次加锁之间目标 Agent 状态变化，
    // 导致消息已按“仅子 Agent 可见”持久化后又回退成主输入。
    let mut locked_team = if agent_id == "main" {
        team.and_then(|team| team.lock().ok())
    } else {
        None
    };
    let message_id = message_id.unwrap_or_else(|| scru128::new().to_string());
    let pending_agent_deliveries = locked_team
        .as_deref()
        .map(|team| {
            crate::agent_team::lifecycle::plan_user_mention_deliveries(team, &message_id, &prepared)
        })
        .unwrap_or_default();
    let route_planned = !pending_agent_deliveries.is_empty();
    let accepted = accept_prepared_user_message_with_options(
        session,
        stream_tx,
        Some(message_id),
        prepared,
        persistence_ack,
        pending_agent_deliveries,
        route_planned,
        !route_planned,
    )?;

    let routed = route_planned
        && locked_team.as_deref_mut().is_some_and(|team| {
            crate::agent_team::lifecycle::dispatch_pending_agent_deliveries(
                team,
                session,
                &accepted.message_id,
                stream_tx,
            )
        });
    if routed {
        Ok(RuntimeMessageDisposition::RoutedToAgent)
    } else {
        if route_planned {
            if let Some(message) = session
                .messages
                .iter_mut()
                .find(|message| message.id == accepted.message_id)
            {
                message.model_excluded = false;
            }
            session.replace_pending_agent_deliveries(&accepted.message_id, Vec::new());
            if let Err(error) = session.try_persist_to_disk() {
                tracing::warn!(%error, "定向消息路由失效后恢复主模型可见性失败");
            }
            emit_session_message_upsert_with_state(
                session,
                stream_tx,
                &accepted.message_id,
                true,
                false,
            );
        }
        Ok(RuntimeMessageDisposition::CurrentAgentInput(
            content_blocks_text(&accepted.prepared),
        ))
    }
}

fn user_message_turn_start_idx(session: &Session, message_id: &str) -> Option<usize> {
    session
        .messages
        .iter()
        .position(|message| message.id == message_id && message.role == MessageRole::User)
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

pub(crate) fn structured_tool_failure_provider_text(record: &ToolFailureRecord) -> String {
    record.render_for_model()
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
        duration_ms: None,
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
        "{error_hint}本轮已经执行过完全相同的 {tool_name} 工具调用且执行失败，系统已跳过重复执行。请不要继续重复相同工具和参数；如果失败原因包含 __parse_error，请重新生成完整 JSON 参数，不要把 __parse_error 当作真实参数；也可以切换到其他可行方式。"
    );
    let _ = stream_tx.send(StreamEvent::ToolResult {
        name: tool_name.to_string(),
        tool_call_id: Some(tool_call_id.to_string()),
        ok: false,
        output: message.clone(),
        full_output: Some(message.clone()),
        duration_ms: None,
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
        let assistant_id = assistant_msg.id.clone();
        let tool_result_id = session.messages.last().map(|message| message.id.clone());
        emit_session_message_upsert(session, stream_tx, &assistant_id);
        if let Some(tool_result_id) = tool_result_id {
            emit_session_message_upsert(session, stream_tx, &tool_result_id);
        }
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
    fn preloaded_same_id_message_uses_its_actual_index_as_turn_start() {
        let mut session = Session::new("preloaded-turn");
        session.append_message(MessageRole::Assistant, "before");
        session.append_prepared_user_message_with_id(
            "preloaded-user".to_string(),
            vec![ContentBlock::text("old content")],
        );
        session.append_message(MessageRole::Assistant, "after");
        let accept_before_len = session.messages.len();

        let message_id = append_or_reuse_user_message(
            &mut session,
            Some("preloaded-user".to_string()),
            vec![ContentBlock::text("new content")],
        )
        .unwrap();
        let turn_start_idx = user_message_turn_start_idx(&session, &message_id).unwrap();

        assert_eq!(turn_start_idx, 1);
        assert_ne!(turn_start_idx, accept_before_len);
        assert_eq!(
            session.messages[turn_start_idx].text_content(),
            "new content"
        );
    }

    #[test]
    fn new_user_message_closes_every_unfinished_tool_call_first() {
        let mut session = Session::new("tool-interruption");
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

        let interrupted = close_unfinished_tool_calls(&mut session);
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].0, "call-2");
        assert!(!has_unfinished_tool_calls(&session));

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

        assert!(has_unfinished_tool_calls(&session));
        append_tool_result_message(
            &mut session,
            "call-pending",
            "write_file",
            "done".to_string(),
            false,
        );
        assert!(!has_unfinished_tool_calls(&session));
    }

    #[test]
    fn reused_tool_call_id_does_not_reuse_an_old_result() {
        let mut session = Session::new("reused-tool-id");
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

        assert!(has_unfinished_tool_calls(&session));
        let interrupted = close_unfinished_tool_calls(&mut session);
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].0, "call-1");
        assert!(!has_unfinished_tool_calls(&session));
    }

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

        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(matches!(
            &events[0],
            StreamEvent::SessionMessageUpsert { message, .. }
                if message.role == MessageRole::Assistant && message.tool_calls.len() == 1
        ));
        assert!(matches!(
            &events[1],
            StreamEvent::SessionMessageUpsert { message, .. }
                if message.role == MessageRole::Tool
        ));
        match &events[2] {
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

    #[test]
    fn structured_tool_failure_renders_argument_guidance() {
        let record = ToolFailureRecord::new(
            "read_file",
            "call_bad",
            "path=(empty)",
            ToolFailureKind::Argument,
            "工具参数 JSON 无效：__parse_error",
        );

        let text = structured_tool_failure_provider_text(&record);

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
