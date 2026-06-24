use crate::app::TiangongApp;
use crate::view::*;
use base64::{engine::general_purpose, Engine as _};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, Window};
use tauri_plugin_notification::{NotificationExt, PermissionState};
use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tracing::{debug, warn};

const MAX_ATTACHMENT_BASE64_BYTES: u64 = 50 * 1024 * 1024;

#[allow(unused_mut)]
fn configure_no_window(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn format_agent_reply_message(agent_label: &str, content: &str) -> String {
    let safe_label = agent_label.replace(['\n', '\r'], " ").replace("--", "- -");
    format!("<!-- tiangong-agent-reply -->\n<!-- label:{safe_label} -->\n\n{content}")
}

fn done_event_keeps_turn_running(
    event: &tiangong_types::StreamEvent,
    has_pending_turn: bool,
) -> bool {
    matches!(event, tiangong_types::StreamEvent::Done { usage: None }) && has_pending_turn
}

#[derive(Debug, serde::Serialize)]
pub struct AttachmentDataUrl {
    pub data_url: String,
    pub mime_type: String,
    pub title: String,
    pub base64_size: u64,
}

#[tauri::command]
pub async fn read_attachment_as_data_url(
    path: String,
    max_base64_bytes: Option<u64>,
    state: State<'_, TiangongApp>,
) -> Result<AttachmentDataUrl, String> {
    ensure_multimodal_enabled(&state).await?;

    let path_buf = PathBuf::from(&path);
    if !path_buf.is_file() {
        return Err(format!("附件不是可读取文件：{path}"));
    }

    let max_base64_bytes = max_base64_bytes.unwrap_or(MAX_ATTACHMENT_BASE64_BYTES);
    let metadata = std::fs::metadata(&path_buf).map_err(|e| format!("读取附件信息失败：{e}"))?;
    let estimated_base64_size = metadata.len().div_ceil(3) * 4;
    if estimated_base64_size > max_base64_bytes {
        return Err(format!(
            "附件过大：base64 编码后约 {:.1}MB，超过 50MB 限制",
            estimated_base64_size as f64 / 1024.0 / 1024.0
        ));
    }

    let bytes = std::fs::read(&path_buf).map_err(|e| format!("读取附件失败：{e}"))?;
    let encoded = general_purpose::STANDARD.encode(bytes);
    if encoded.len() as u64 > max_base64_bytes {
        return Err("附件过大：base64 编码后超过 50MB 限制".to_string());
    }

    let mime_type = mime_type_from_path(&path_buf);
    let title = path_buf
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path.as_str())
        .to_string();
    Ok(AttachmentDataUrl {
        data_url: format!("data:{mime_type};base64,{encoded}"),
        mime_type,
        title,
        base64_size: encoded.len() as u64,
    })
}

fn mime_type_from_path(path: &std::path::Path) -> String {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "txt" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// 从消息的 content blocks 提取 media 资产（新格式）。
///
/// `append_message_with_id_and_media` 把附件存进 `content` 的 `ContentBlock::Media`，
/// 而非旧的 `media` 字段。提取时必须从 content blocks 取，否则附件会丢失。
fn extract_media_from_content(
    message: &tiangong_types::Message,
) -> Vec<tiangong_types::MediaAsset> {
    message
        .content
        .iter()
        .filter_map(|block| {
            if let tiangong_types::message::ContentBlock::Media {
                kind,
                url,
                mime_type,
                title,
            } = block
            {
                Some(tiangong_types::MediaAsset {
                    kind: *kind,
                    url: url.clone(),
                    mime_type: mime_type.clone(),
                    title: title.clone(),
                    capability: None,
                })
            } else {
                None
            }
        })
        .collect()
}

async fn ensure_multimodal_enabled(state: &State<'_, TiangongApp>) -> Result<(), String> {
    state
        .with_state_read(|core_state| {
            let enabled = has_capability_in_state(
                core_state,
                tiangong_core::models_config::ModelCapability::Multimodal,
            );
            if enabled {
                Ok(())
            } else {
                Err(anyhow::anyhow!("未配置多模态模型，文件上传能力已关闭。"))
            }
        })
        .await
}

#[tauri::command]
pub async fn request_desktop_notification_permission(app: AppHandle) -> Result<bool, String> {
    let permission = app
        .notification()
        .request_permission()
        .map_err(|err| err.to_string())?;
    Ok(matches!(permission, PermissionState::Granted))
}

#[tauri::command]
pub async fn send_desktop_notification(
    title: String,
    body: String,
    session_id: Option<String>,
    app: AppHandle,
) -> Result<bool, String> {
    let permission = app
        .notification()
        .request_permission()
        .map_err(|err| err.to_string())?;
    if !matches!(permission, PermissionState::Granted) {
        return Ok(false);
    }

    let _ = session_id;
    show_desktop_notification(&app, title, body, "tiangong-background-sessions")?;
    Ok(true)
}

fn show_desktop_notification(
    app: &AppHandle,
    title: String,
    body: String,
    group: &str,
) -> Result<(), String> {
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .group(group)
        .auto_cancel()
        .show()
        .map_err(|err| err.to_string())
}

fn main_window_is_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false)
}

async fn send_approval_notification_if_background(
    app: AppHandle,
    session_id: String,
    request_id: String,
    tool_name: String,
    args_summary: String,
) {
    let is_current_session = app
        .state::<TiangongApp>()
        .with_state_read(|state| Ok(state.active_session_id() == session_id))
        .await
        .unwrap_or(false);
    if is_current_session && main_window_is_focused(&app) {
        return;
    }

    let permission = app.notification().request_permission();
    if !matches!(permission, Ok(PermissionState::Granted)) {
        return;
    }

    let title = "天工 - 需要审批".to_string();
    let body = if args_summary.trim().is_empty() {
        format!("工具 {tool_name} 等待同意或拒绝")
    } else {
        format!("{tool_name}: {args_summary}")
    };

    let _ = request_id;
    let _ = show_desktop_notification(&app, title, body, "tiangong-approval-requests");
}

fn ensure_assistant_message(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
    message_id: &str,
) {
    if !session.messages.iter().any(|msg| msg.id == message_id) {
        session.append_message_with_id(
            message_id.to_string(),
            tiangong_core::session::MessageRole::Assistant,
            String::new(),
            String::new(),
        );
    }

    *assistant_msg_id = Some(message_id.to_string());
}

fn append_assistant_delta(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
    message_id: &str,
    content: &str,
) {
    if content.trim().is_empty() && !session.messages.iter().any(|msg| msg.id == message_id) {
        return;
    }
    ensure_assistant_message(session, assistant_msg_id, message_id);
    if let Some(msg) = session.messages.iter_mut().find(|msg| msg.id == message_id) {
        if msg.text_content().trim().is_empty() && content.trim().is_empty() {
            return;
        }
        match msg.content.last_mut() {
            Some(tiangong_types::ContentBlock::Text { text }) => text.push_str(content),
            _ => msg
                .content
                .push(tiangong_types::ContentBlock::text(content.to_string())),
        }
    }
}

fn append_assistant_reasoning(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
    message_id: &str,
    content: &str,
) {
    ensure_assistant_message(session, assistant_msg_id, message_id);
    if let Some(msg) = session.messages.iter_mut().find(|msg| msg.id == message_id) {
        msg.reasoning_content.push_str(content);
    }
}

fn cleanup_assistant_before_tool_calls(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
) {
    let Some(message_id) = assistant_msg_id.take() else {
        return;
    };
    let Some(index) = session.messages.iter().position(|msg| {
        msg.id == message_id && msg.role == tiangong_core::session::MessageRole::Assistant
    }) else {
        return;
    };

    let message = &mut session.messages[index];
    if !message.text_content().trim().is_empty() {
        return;
    }
    message.content.clear();
    if message.reasoning_content.trim().is_empty() && !message.has_media() {
        session.messages.remove(index);
    }
}

fn finalize_assistant_tool_calls(
    session: &mut tiangong_core::session::Session,
    assistant_msg_id: &mut Option<String>,
    message_id: &str,
    calls: &[tiangong_types::StreamToolCall],
) {
    if calls.is_empty() {
        cleanup_assistant_before_tool_calls(session, assistant_msg_id);
        return;
    }
    ensure_assistant_message(session, assistant_msg_id, message_id);
    if let Some(msg) = session.messages.iter_mut().find(|msg| msg.id == message_id) {
        msg.tool_calls = calls
            .iter()
            .map(|call| tiangong_core::session::MessageToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect();
    }
    *assistant_msg_id = None;
}

fn append_tool_result_message(
    session: &mut tiangong_core::session::Session,
    tool_call_id: Option<&str>,
    tool_name: &str,
    content: String,
    is_error: bool,
) {
    let Some(tool_call_id) = tool_call_id else {
        return;
    };
    let mut message =
        tiangong_core::session::Message::new(tiangong_core::session::MessageRole::Tool, content);
    message.tool_call_id = Some(tool_call_id.to_string());
    message.tool_name = Some(tool_name.to_string());
    message.tool_result_is_error = is_error;
    session.messages.push(message);
    session.updated_at = tiangong_core::session::now_text();
}

fn append_assistant_media(
    session: &mut tiangong_core::session::Session,
    media: Vec<tiangong_types::MediaAsset>,
) {
    session.append_message_with_media(
        tiangong_core::session::MessageRole::Assistant,
        String::new(),
        media,
    );
}

fn record_session_token_usage(
    session: &mut tiangong_core::session::Session,
    usage: &tiangong_types::TokenUsage,
    current_tokens: Option<usize>,
    compression_threshold_tokens: Option<usize>,
    context_limit_tokens: Option<usize>,
    agent_id: Option<&str>,
) {
    let mut normalized_usage = usage.clone();
    if normalized_usage.total_tokens == 0 {
        normalized_usage.total_tokens =
            normalized_usage.prompt_tokens + normalized_usage.completion_tokens;
    }
    if normalized_usage.total_tokens > 0 {
        session.token_usage.accumulate(&normalized_usage);
        if let Some(aid) = agent_id {
            session
                .agent_token_usage
                .entry(aid.to_string())
                .or_default()
                .accumulate(&normalized_usage);
        }
    }
    if let Some(current_tokens) = current_tokens {
        if let Some(aid) = agent_id {
            session.active_agent_id = Some(aid.to_string());
            session.active_agent_current_tokens =
                current_tokens.max(session.active_agent_current_tokens);
            let agent_current_tokens = session
                .agent_current_tokens
                .entry(aid.to_string())
                .or_default();
            *agent_current_tokens = current_tokens.max(*agent_current_tokens);
        } else {
            session.current_tokens = current_tokens.max(session.current_tokens);
        }
    }
    if let Some(compression_threshold_tokens) = compression_threshold_tokens {
        session.compression_threshold_tokens = compression_threshold_tokens;
    }
    if let Some(context_limit_tokens) = context_limit_tokens {
        session.context_limit_tokens = context_limit_tokens;
    }
    session.updated_at = tiangong_core::session::now_text();
}

fn parse_model_capability(
    capability: &str,
) -> Result<tiangong_core::models_config::ModelCapability, String> {
    tiangong_core::models_config::ModelCapability::from_key(capability)
        .ok_or_else(|| format!("不支持的能力类型：{capability}"))
}

fn has_capability_in_state(
    core_state: &tiangong_core::app_state::TiangongState,
    capability: tiangong_core::models_config::ModelCapability,
) -> bool {
    core_state.models_config().has_capability(capability)
}

// ============================================================================
// 辅助函数：构建完整的 RunSnapshot
// ============================================================================

pub fn build_full_snapshot_with_status(
    core_state: &tiangong_core::app_state::TiangongState,
    is_executing: bool,
) -> RunSnapshotView {
    let sid = core_state.active_session_id();
    build_session_snapshot(core_state, sid, is_executing)
}

fn build_session_snapshot(
    core_state: &tiangong_core::app_state::TiangongState,
    session_id: &str,
    is_session_executing: bool,
) -> RunSnapshotView {
    let core_snapshot = core_state.run_snapshot();
    let input_draft = core_state.input_draft().to_string();

    let selected_session = core_state.sessions().iter().find(|s| s.id == session_id);

    let messages: Vec<tiangong_types::Message> = selected_session
        .map(|s| s.messages.clone())
        .unwrap_or_default();

    let current_plan = core_state
        .active_task_plans()
        .first()
        .map(TaskPlan::from_session_task_plan);

    let pending_session_ids = core_state.pending_session_ids();

    let mut snapshot = RunSnapshotView::from_core_with_session(
        core_snapshot,
        messages,
        input_draft,
        current_plan,
        pending_session_ids,
        selected_session
            .map(TokenStatsView::from_session)
            .unwrap_or_default(),
    );
    snapshot.last_usage = selected_session.and_then(|session| {
        let usage = session.total_usage();
        (usage.total_tokens > 0).then_some(usage)
    });

    // 按 session 独立判断状态
    if is_session_executing {
        // 该 session 有活跃的 TiangongCore
        if snapshot.last_session_id.as_deref() != Some(session_id) {
            snapshot.status = tiangong_types::RunStatus::Executing;
            snapshot.summary = "正在处理".to_string();
        }
    } else {
        // 该 session 没有活跃 core → idle
        snapshot.status = tiangong_types::RunStatus::Idle;
        snapshot.current_plan = None;
    }

    snapshot
}

// ============================================================================
// 会话管理
// ============================================================================

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionTabsView {
    pub tabs: Vec<tiangong_core::session::TabState>,
    pub active_tab_id: Option<String>,
}

/// 获取所有会话列表
#[tauri::command]
pub async fn get_sessions(state: State<'_, TiangongApp>) -> Result<Vec<SessionListItem>, String> {
    state
        .with_state_read(|core_state| {
            Ok(core_state
                .sessions()
                .iter()
                .filter(|session| session.parent_session_id.is_none())
                .map(SessionListItem::from_core)
                .collect())
        })
        .await
}

/// 获取指定会话的统一工作区 Tab 元数据
#[tauri::command]
pub async fn get_session_tabs(
    session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<SessionTabsView, String> {
    state
        .with_state_read(|core_state| {
            let session = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("会话不存在：{session_id}"))?;
            Ok(SessionTabsView {
                tabs: session.tabs.clone(),
                active_tab_id: session.active_tab_id.clone(),
            })
        })
        .await
}

/// 写入指定会话的统一工作区 Tab 元数据
#[tauri::command]
pub async fn set_session_tabs(
    session_id: String,
    tabs: Vec<tiangong_core::session::TabState>,
    active_tab_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    state
        .with_state(|core_state| {
            let session = core_state
                .sessions_mut()
                .iter_mut()
                .find(|session| session.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("会话不存在：{session_id}"))?;
            session.tabs = tabs;
            session.active_tab_id = active_tab_id;
            core_state.persist_session_and_app(&session_id)
        })
        .await
}

/// 创建新会话
#[tauri::command]
pub async fn create_session(state: State<'_, TiangongApp>) -> Result<SessionListItem, String> {
    let result = state
        .with_state(|core_state| {
            core_state.create_session();
            // 返回新创建的活动会话
            core_state
                .active_session()
                .map(SessionListItem::from_core)
                .ok_or_else(|| anyhow::anyhow!("Failed to create session"))
        })
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(result)
}

/// 切换到指定会话
#[tauri::command]
pub async fn switch_session(
    session_id: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    state
        .with_state(|core_state| {
            core_state.switch_session(&session_id);
            Ok(())
        })
        .await?;
    state.sync_core_config_from_state().await?;
    let trust_mode = state
        .with_state_read(|core_state| Ok(core_state.active_session_trust_mode()))
        .await?;
    state.set_core_trust_mode(&session_id, trust_mode);

    // 为新会话补充索引（后台执行，不阻塞 UI）
    let cwd = state
        .with_state_read(|core_state| Ok(core_state.active_session_effective_cwd()))
        .await?;
    let sid = session_id.clone();
    let has_session_index = tiangong_core::index::session_index_exists(&sid);
    let messages = if !has_session_index {
        state
            .with_state_read(|core_state| {
                Ok(core_state
                    .sessions()
                    .iter()
                    .find(|s| s.id == sid)
                    .map(|s| s.messages.clone())
                    .unwrap_or_default())
            })
            .await?
    } else {
        Vec::new()
    };

    if !cwd.is_empty() || !messages.is_empty() {
        let app_clone = app.clone();
        let rt = tokio::runtime::Handle::current();
        thread::spawn(move || {
            let mut need_snapshot = false;

            // Workspace 索引：仅在尚不存在时创建
            if !cwd.is_empty()
                && !tiangong_core::index::workspace_index_exists(std::path::Path::new(&cwd))
            {
                match tiangong_core::index::rebuild_workspace_index_for_gui(std::path::Path::new(
                    &cwd,
                )) {
                    Ok(count) => {
                        debug!(count, "切换会话后 Workspace 索引扫描完成");
                        need_snapshot = true;
                    }
                    Err(e) => {
                        warn!(error = %e, "切换会话后 Workspace 索引扫描失败");
                    }
                }
            }

            // Session 索引：回溯已有消息
            if !messages.is_empty() {
                match tiangong_core::index::backfill_session_index(&sid, &messages) {
                    Ok(count) => {
                        debug!(count, "切换会话后 Session 回溯索引完成");
                        need_snapshot = true;
                    }
                    Err(e) => {
                        warn!(error = %e, "切换会话后 Session 回溯索引失败");
                    }
                }
            }

            if need_snapshot {
                if let Ok(snapshot) = rt.block_on(
                    app_clone
                        .state::<TiangongApp>()
                        .with_state_read(|s| Ok(build_full_snapshot_with_status(s, false))),
                ) {
                    let _ = app_clone.emit("run_snapshot", &snapshot);
                }
            }
        });
    }

    Ok(())
}

/// 删除当前会话
#[tauri::command]
pub async fn delete_session(app: AppHandle, state: State<'_, TiangongApp>) -> Result<(), String> {
    let deleted_id = state
        .with_state(|core_state| {
            // 在 delete_active_session 清空 active_session_id 之前先捕获，
            // 以便随后销毁该对话专属的交互 PTY，避免进程泄漏
            let id = core_state.active_session_id().to_string();
            core_state.delete_active_session()?;
            Ok::<String, anyhow::Error>(id)
        })
        .await?;
    // 删除对话后销毁其交互 PTY（drop cmd_tx → 命令循环退出 → 子进程终止）
    tiangong_plugin_terminal::destroy_session_pty(&app, &deleted_id);
    Ok(())
}

/// 更新会话标题
#[tauri::command]
pub async fn update_session_title(
    title: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    state
        .with_state(|core_state| {
            core_state.update_session_title_draft(title);
            core_state.save_active_session_title()
        })
        .await
}

// ============================================================================
// 消息和执行
// ============================================================================

/// 发送消息并执行
#[tauri::command]
pub async fn send_message(
    content: String,
    app: AppHandle,
    _window: Window,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    send_message_inner(content, Vec::new(), app, state).await
}

#[tauri::command]
pub async fn send_message_with_media(
    content: String,
    media: Vec<tiangong_types::MediaAsset>,
    app: AppHandle,
    _window: Window,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    if parse_context_slash_command(&content).is_none() && !media.is_empty() {
        ensure_multimodal_enabled(&state).await?;
    }
    send_message_inner(content, media, app, state).await
}

async fn send_message_inner(
    content: String,
    media: Vec<tiangong_types::MediaAsset>,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    use std::sync::mpsc;
    use tiangong_types::SessionStreamEvent;

    if let Some(command) = parse_context_slash_command(&content) {
        run_context_slash_command(command, app, &state).await?;
        return Ok(());
    }

    state.sync_core_config_from_state().await?;

    // 准备 session
    let (session_id, user_message_id, session_snapshot) = state
        .with_state(|core_state| {
            core_state.prepare_active_user_message_ingress_with_media(content.clone(), media)
        })
        .await?;
    // 从 content blocks 提取 media（新格式），而非旧 media 字段。
    // append_message_with_id_and_media 把 media 存进 content blocks，
    // 旧 media 字段为空。若取旧字段会导致附件丢失（issue #149）。
    let command_media = session_snapshot
        .messages
        .iter()
        .find(|message| message.id == user_message_id)
        .map(extract_media_from_content)
        .unwrap_or_default();

    // 获取或创建 TiangongCore
    let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();
    let (sid, is_new_core) = state.ensure_core(&session_id, session_snapshot, stream_tx);
    // 发送消息（core 内部会 append 到 core session 并推送 UserMessage 事件）
    {
        let cores = state.cores.lock().map_err(|e| e.to_string())?;
        if let Some(core) = cores.get(&sid) {
            if !core.deliver(AgentInputKind::message_with_id(
                content.clone(),
                user_message_id,
                command_media,
            )) {
                return Err("会话 core 已停止，请重试发送".to_string());
            }
        }
    }

    // 只在新创建 core 时启动消费线程（复用 core 时旧消费线程仍在运行）
    if !is_new_core {
        return Ok(());
    }

    let cancel_flag = get_cancel_flag(&state, &sid)?;
    start_stream_consumer(app, stream_rx, cancel_flag);

    Ok(())
}

fn get_cancel_flag(
    state: &TiangongApp,
    sid: &str,
) -> Result<Arc<std::sync::atomic::AtomicBool>, String> {
    let cores = state.cores.lock().map_err(|e| e.to_string())?;
    Ok(cores
        .get(sid)
        .map(|c| c.cancel_flag())
        .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false))))
}

/// 消费 SessionStreamEvent：emit 给前端 + 更新 RunStatus + Done 时同步 session
pub(crate) fn start_stream_consumer(
    app: AppHandle,
    stream_rx: std::sync::mpsc::Receiver<tiangong_types::SessionStreamEvent>,
    cancel_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    use tiangong_types::StreamEvent;

    let rt = tokio::runtime::Handle::current();
    thread::spawn(move || {
        let mut assistant_msg_id: Option<String> = None;
        let mut last_tool_args_summary = String::new();
        for session_event in stream_rx.iter() {
            let event = &session_event.event;

            // 取消时跳过文本增量事件，只处理终止事件
            if cancel_flag.load(Ordering::Acquire)
                && matches!(
                    event,
                    StreamEvent::Delta { .. } | StreamEvent::Reasoning { .. }
                )
            {
                continue;
            }

            // 先 emit 完整事件给前端，再解构
            let _ = app.emit("stream_event", &session_event);

            let sid = session_event.session_id;
            let event = session_event.event;
            let is_done = matches!(event, StreamEvent::Done { .. });
            let is_error = matches!(event, StreamEvent::Error { .. });

            // 更新 session + RunStatus/usage
            let _ = rt.block_on(app.state::<TiangongApp>().with_state(|core_state| {
                if let Some(session) = core_state.sessions_mut().iter_mut().find(|s| s.id == sid) {
                    match &event {
                        StreamEvent::UserMessage {
                            message_id,
                            content,
                            media,
                        } => {
                            // Core 已记录用户消息，同步到 TiangongState session
                            if !session.messages.iter().any(|msg| msg.id == *message_id) {
                                session.append_message_with_id_and_media(
                                    message_id.clone(),
                                    tiangong_core::session::MessageRole::User,
                                    content.clone(),
                                    String::new(),
                                    media.clone(),
                                );
                            } else if !media.is_empty() {
                                if let Some(message) = session
                                    .messages
                                    .iter_mut()
                                    .find(|msg| msg.id == *message_id)
                                {
                                    if message.media.is_empty() {
                                        message.media = media.clone();
                                    }
                                }
                            }
                        }
                        StreamEvent::Delta {
                            message_id,
                            content,
                        } => {
                            append_assistant_delta(
                                session,
                                &mut assistant_msg_id,
                                message_id,
                                content,
                            );
                        }
                        StreamEvent::Reasoning {
                            message_id,
                            content,
                        } => {
                            append_assistant_reasoning(
                                session,
                                &mut assistant_msg_id,
                                message_id,
                                content,
                            );
                        }
                        StreamEvent::ToolCalls {
                            message_id,
                            names,
                            calls,
                            usage: _,
                        } => {
                            finalize_assistant_tool_calls(
                                session,
                                &mut assistant_msg_id,
                                message_id,
                                calls,
                            );
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!("LLM 输出\ntool_calls: {}", names.join(", ")),
                            );
                        }
                        StreamEvent::TokenUsage {
                            usage,
                            current_tokens,
                            compression_threshold_tokens,
                            context_limit_tokens,
                            agent_id,
                            ..
                        } => {
                            record_session_token_usage(
                                session,
                                usage,
                                *current_tokens,
                                *compression_threshold_tokens,
                                *context_limit_tokens,
                                agent_id.as_deref(),
                            );
                        }
                        StreamEvent::ToolStart {
                            ref args_summary, ..
                        } => {
                            // 不清除 pending_final_media，允许多轮工具调用后仍保留已生成的媒体
                            last_tool_args_summary = args_summary.clone();
                        }
                        StreamEvent::ToolResult {
                            ref name,
                            ref tool_call_id,
                            ok,
                            ref output,
                            ref full_output,
                            ref media,
                        } => {
                            let persisted_output = full_output.as_deref().unwrap_or(output);
                            let status = if *ok { "ok=true" } else { "ok=false" };

                            // plugin_injection 注入结果：追加完整消息对（与 worker session 一致）
                            if name == tiangong_core::react::message::INJECTION_TOOL_NAME {
                                use tiangong_core::session::{Message, MessageRole};
                                let tc_id = tool_call_id.clone().unwrap_or_default();
                                let mut assistant_msg =
                                    Message::new(MessageRole::Assistant, String::new());
                                assistant_msg.tool_calls =
                                    vec![tiangong_core::session::MessageToolCall {
                                        id: tc_id.clone(),
                                        name: name.clone(),
                                        arguments: serde_json::json!({}),
                                    }];
                                session.messages.push(assistant_msg);
                                append_tool_result_message(
                                    session,
                                    Some(&tc_id),
                                    name,
                                    persisted_output.to_string(),
                                    !*ok,
                                );
                            } else {
                                // 正常工具结果：System 摘要 + Tool result
                                let mut lines = vec![format!("工具执行 [{name}]")];
                                if !last_tool_args_summary.is_empty() {
                                    lines.push(format!("命令: {last_tool_args_summary}"));
                                }
                                lines.push(format!("{status} exit_code=0"));
                                lines.push(format!("summary: {name}"));
                                if !media.is_empty() {
                                    let media_desc = media
                                        .iter()
                                        .map(|a| match a.kind {
                                            tiangong_types::MediaKind::Image => "图片",
                                            tiangong_types::MediaKind::Video => "视频",
                                            tiangong_types::MediaKind::Audio => "音频",
                                            _ => "文件",
                                        })
                                        .next()
                                        .unwrap_or("媒体");
                                    let count = media.len();
                                    lines.push(format!("stdout: 已生成 {count} 个{media_desc}"));
                                    append_assistant_media(session, media.clone());
                                } else if !persisted_output.trim().is_empty() {
                                    lines.push(format!("stdout:\n{persisted_output}"));
                                }
                                session.append_message(
                                    tiangong_types::MessageRole::System,
                                    lines.join("\n"),
                                );
                                append_tool_result_message(
                                    session,
                                    tool_call_id.as_deref(),
                                    name,
                                    persisted_output.to_string(),
                                    !*ok,
                                );
                            }
                            last_tool_args_summary.clear();
                            let _ = core_state.persist_session_and_app(&sid);
                        }
                        StreamEvent::ApprovalNeeded { .. } => {
                            // 审批请求不写入 session（前端通过 RunStatus 展示审批 UI）
                        }
                        StreamEvent::Error { ref message } => {
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!("[错误] {message}"),
                            );
                            assistant_msg_id = None;
                        }
                        StreamEvent::Retry {
                            ref message,
                            attempt,
                            max_attempts,
                        } => {
                            let _ = (message, attempt, max_attempts);
                        }
                        StreamEvent::Done { .. } => {
                            assistant_msg_id = None;
                        }
                        StreamEvent::MemoryRecallStart { ref strategy } => {
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!("[记忆检索] 策略: {strategy}"),
                            );
                        }
                        StreamEvent::MemoryRecallDone {
                            hit_count,
                            ref hits,
                        } => {
                            if *hit_count == 0 {
                                session.append_message(
                                    tiangong_core::session::MessageRole::System,
                                    "[记忆检索] 无相关记忆".to_string(),
                                );
                            } else {
                                let items: Vec<String> = hits
                                    .iter()
                                    .map(|h| {
                                        format!("- [{:.2}] {}: {}", h.score, h.title, h.summary)
                                    })
                                    .collect();
                                session.append_message(
                                    tiangong_core::session::MessageRole::System,
                                    format!(
                                        "[记忆检索] 命中 {} 条\n{}",
                                        hit_count,
                                        items.join("\n")
                                    ),
                                );
                            }
                        }
                        StreamEvent::AgentCreated {
                            ref agent_id,
                            ref role,
                            ref label,
                        } => {
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!("[Agent] {label} ({role}) 已加入团队 id={agent_id}"),
                            );
                        }
                        StreamEvent::AgentStatusChanged {
                            ref agent_id,
                            ref label,
                            ref status,
                        } => {
                            if status == "terminated" {
                                session.agent_current_tokens.remove(agent_id);
                                session.agent_token_usage.remove(agent_id);
                            }
                            if (status == "idle" || status == "terminated")
                                && session.active_agent_id.as_deref() == Some(agent_id.as_str())
                            {
                                session.active_agent_id = None;
                                session.active_agent_current_tokens = 0;
                            }
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!("[Agent] {label} 状态变更: {status} id={agent_id}"),
                            );
                        }
                        StreamEvent::AgentNotification {
                            ref agent_label,
                            ref content,
                            ..
                        } => {
                            session.append_message(
                                tiangong_core::session::MessageRole::Assistant,
                                format_agent_reply_message(agent_label, content),
                            );
                        }
                        StreamEvent::AgentMessage {
                            ref from_agent_id,
                            ref from_agent_label,
                            ref to_agent_id,
                            ref content,
                            ..
                        } => {
                            if to_agent_id == "main" && from_agent_id != "user" {
                                session.append_message(
                                    tiangong_core::session::MessageRole::Assistant,
                                    format_agent_reply_message(from_agent_label, content),
                                );
                            } else if from_agent_id == "user" {
                                // 用户 @Agent 的原始输入已经作为用户消息存在，避免重复写入。
                            }
                        }
                        StreamEvent::AgentOutput {
                            ref agent_id,
                            ref agent_role,
                            ref agent_label,
                            ref messages,
                        } => {
                            let worker_id = format!("agent:{agent_role}:{agent_id}");
                            let header = format!("🔧 Worker: {agent_label} (@{agent_role})");
                            if !session.messages.iter().any(|message| {
                                message.worker_id.as_deref() == Some(worker_id.as_str())
                                    && message.text_content() == header
                            }) {
                                session.append_worker_message(
                                    tiangong_core::session::MessageRole::System,
                                    header,
                                    &worker_id,
                                );
                            }
                            for message in messages {
                                let role = match message.role {
                                    tiangong_core::session::MessageRole::Assistant => {
                                        tiangong_core::session::MessageRole::Assistant
                                    }
                                    tiangong_core::session::MessageRole::System
                                    | tiangong_core::session::MessageRole::Tool => {
                                        tiangong_core::session::MessageRole::System
                                    }
                                    tiangong_core::session::MessageRole::User => {
                                        tiangong_core::session::MessageRole::User
                                    }
                                };

                                if let Some(existing) = session.messages.iter_mut().find(|item| {
                                    item.id == message.id
                                        && item.worker_id.as_deref() == Some(worker_id.as_str())
                                }) {
                                    if role == tiangong_core::session::MessageRole::Assistant {
                                        for block in &message.content {
                                            existing.content.push(block.clone());
                                        }
                                        existing
                                            .reasoning_content
                                            .push_str(&message.reasoning_content);
                                    }
                                    continue;
                                }

                                let mut worker_message = message.clone();
                                worker_message.role = role;
                                worker_message.worker_id = Some(worker_id.clone());
                                session.messages.push(worker_message);
                                session.updated_at = tiangong_core::session::now_text();
                            }
                        }
                        StreamEvent::FileLockChanged {
                            ref path,
                            ref holder_agent_label,
                            ref action,
                            ..
                        } => {
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                format!(
                                    "[文件锁] {path} {action} by {}",
                                    holder_agent_label.as_deref().unwrap_or("未知")
                                ),
                            );
                        }
                        StreamEvent::ContextCompressing { .. } => {}
                        StreamEvent::ContextCompressed {
                            ref action,
                            summary_up_to,
                            ..
                        } => {
                            let message = format!("[上下文管理] 上下文已{}", action.display_text());
                            session.append_message(
                                tiangong_core::session::MessageRole::System,
                                message,
                            );
                            // 同步 core worker 中对 session 字段的修改
                            session.summary_up_to = *summary_up_to;
                            tiangong_core::context::compressor::mark_compact_boundary(
                                &mut session.messages,
                                *summary_up_to,
                            );
                            session.current_tokens = 0;
                            session.active_agent_current_tokens = 0;
                            session.agent_current_tokens.clear();
                            if *action == tiangong_types::stream::ContextCompressAction::Clear {
                                session.context_summary = None;
                            }
                            let _ = core_state.persist_session_and_app(&sid);
                        }
                        _ => {}
                    }
                }
                // RunStatus/usage 更新
                match &event {
                    StreamEvent::ApprovalNeeded {
                        ref request_id,
                        ref tool_name,
                        ref args_summary,
                    } => {
                        core_state.store.runtime.run.status =
                            tiangong_core::runtime::RunStatus::WaitingApproval;
                        core_state.store.runtime.run.summary = if args_summary.is_empty() {
                            format!("工具 {tool_name} 需要确认")
                        } else {
                            format!("{tool_name}: {args_summary}")
                        };
                        core_state.store.runtime.run.approval_request_id = Some(request_id.clone());
                    }
                    StreamEvent::ToolStart {
                        name,
                        ref args_summary,
                    } => {
                        // 审批通过后恢复执行状态
                        core_state.store.runtime.run.status =
                            tiangong_core::runtime::RunStatus::Executing;
                        core_state.store.runtime.run.approval_request_id = None;
                        core_state.store.runtime.run.summary = if args_summary.is_empty() {
                            format!("正在执行：{name}")
                        } else {
                            format!("正在执行：{name} {args_summary}")
                        };
                    }
                    StreamEvent::ToolResult { name, ok, .. } => {
                        let s = if *ok { "✓" } else { "✗" };
                        core_state.store.runtime.run.summary = format!("{s} {name}");
                    }
                    StreamEvent::ToolCalls { names, .. } => {
                        core_state.store.runtime.run.summary =
                            format!("正在执行：{}", names.join(", "));
                    }
                    StreamEvent::TokenUsage { .. } => {
                        if let Some(session) = core_state.sessions().iter().find(|s| s.id == sid) {
                            let total = session.total_usage();
                            core_state.store.runtime.run.last_usage =
                                (total.total_tokens > 0).then_some(total);
                        }
                    }
                    StreamEvent::Done { .. } => {
                        if done_event_keeps_turn_running(
                            &event,
                            core_state.has_pending_turn_for(&sid),
                        ) {
                            core_state.store.runtime.run.status =
                                tiangong_core::runtime::RunStatus::Executing;
                            core_state.store.runtime.run.summary = "正在处理".to_string();
                            core_state.store.runtime.run.last_session_id = Some(sid.clone());
                            core_state.store.runtime.run.updated_at =
                                tiangong_core::session::now_text();
                        } else {
                            core_state.report_run_idle(format!(
                                "模型供应商：{}",
                                core_state.provider_label()
                            ));
                            core_state.clear_pending_turn_for(&sid);
                        }
                    }
                    StreamEvent::Error { ref message } => {
                        core_state.report_run_idle(format!("执行失败：{message}"));
                        core_state.clear_pending_turn_for(&sid);
                    }
                    StreamEvent::Retry {
                        attempt,
                        max_attempts,
                        ..
                    } => {
                        core_state.store.runtime.run.summary =
                            format!("重试中 ({attempt}/{max_attempts})...");
                    }
                    StreamEvent::Reasoning { .. } => {
                        core_state.store.runtime.run.summary = "正在思考...".to_string();
                    }
                    StreamEvent::Delta { .. } => {
                        core_state.store.runtime.run.summary = "正在回复...".to_string();
                    }
                    StreamEvent::MemoryRecallStart { .. } => {
                        core_state.store.runtime.run.summary = "正在检索记忆...".to_string();
                    }
                    StreamEvent::MemoryRecallProgress { ref phase } => {
                        core_state.store.runtime.run.summary = format!("正在检索记忆: {phase}");
                    }
                    StreamEvent::MemoryRecallDone { hit_count, .. } => {
                        if *hit_count > 0 {
                            core_state.store.runtime.run.summary =
                                format!("记忆检索完成，命中 {hit_count} 条");
                        } else {
                            core_state.store.runtime.run.summary =
                                "记忆检索完成，无相关记忆".to_string();
                        }
                    }
                    StreamEvent::AgentCreated { ref label, .. } => {
                        core_state.store.runtime.run.summary = format!("Agent {label} 已加入团队");
                    }
                    StreamEvent::AgentStatusChanged {
                        ref label,
                        ref status,
                        ..
                    } => {
                        core_state.store.runtime.run.summary = format!("Agent {label}: {status}");
                    }
                    StreamEvent::AgentNotification {
                        ref agent_label, ..
                    } => {
                        core_state.store.runtime.run.summary =
                            format!("Agent {agent_label} 发送了通知");
                    }
                    StreamEvent::AgentMessage {
                        ref from_agent_label,
                        ref to_agent_label,
                        ..
                    } => {
                        core_state.store.runtime.run.summary =
                            format!("{from_agent_label} → {to_agent_label}");
                    }
                    StreamEvent::AgentOutput {
                        ref agent_label, ..
                    } => {
                        core_state.store.runtime.run.summary =
                            format!("Agent {agent_label} 输出已更新");
                    }
                    StreamEvent::FileLockChanged {
                        ref path,
                        ref action,
                        ref holder_agent_label,
                        ..
                    } => {
                        core_state.store.runtime.run.summary = format!(
                            "文件锁 {action}: {path} ({})",
                            holder_agent_label.as_deref().unwrap_or("未知")
                        );
                    }
                    StreamEvent::ContextCompressing { .. } => {
                        core_state.store.runtime.run.status =
                            tiangong_core::runtime::RunStatus::Executing;
                        core_state.store.runtime.run.summary = "正在压缩早期上下文...".to_string();
                        core_state.store.runtime.run.last_session_id = Some(sid.clone());
                        core_state.store.runtime.run.updated_at =
                            tiangong_core::session::now_text();
                    }
                    StreamEvent::ContextCompressed { ref action, .. } => {
                        let text = format!("上下文{}", action.display_text());
                        if core_state.has_pending_turn_for(&sid) {
                            core_state.store.runtime.run.summary = text;
                        } else {
                            core_state.report_run_idle(text);
                        }
                    }
                    StreamEvent::IndexStatus { ref phase, count } => match phase.as_str() {
                        "scanning" => {
                            core_state.store.runtime.run.summary =
                                "正在建立工作区索引...".to_string();
                        }
                        "done" => {
                            core_state.store.runtime.run.summary =
                                format!("索引扫描完成: {count} 个文件");
                        }
                        "error" => {
                            core_state.store.runtime.run.summary = "索引扫描失败".to_string();
                        }
                        _ => {}
                    },
                    _ => {}
                }
                Ok(())
            }));

            // emit run_snapshot
            {
                let is_context_event = matches!(
                    &event,
                    StreamEvent::ContextCompressing { .. } | StreamEvent::ContextCompressed { .. }
                );
                let keeps_running = if is_done {
                    rt.block_on(app.state::<TiangongApp>().with_state_read(|s| {
                        Ok(done_event_keeps_turn_running(
                            &event,
                            s.has_pending_turn_for(&sid),
                        ))
                    }))
                    .unwrap_or(false)
                } else {
                    false
                };
                let is_exec = if keeps_running {
                    true
                } else if is_done || is_error {
                    false
                } else if matches!(&event, StreamEvent::ContextCompressing { .. }) {
                    true
                } else if is_context_event {
                    rt.block_on(
                        app.state::<TiangongApp>()
                            .with_state_read(|s| Ok(s.has_pending_turn_for(&sid))),
                    )
                    .unwrap_or(false)
                } else {
                    true
                };
                if let Ok(snapshot) = rt.block_on(
                    app.state::<TiangongApp>()
                        .with_state_read(|s| Ok(build_full_snapshot_with_status(s, is_exec))),
                ) {
                    let _ = app.emit("run_snapshot", &snapshot);
                }
            }

            if let StreamEvent::ApprovalNeeded {
                request_id,
                tool_name,
                args_summary,
            } = &event
            {
                rt.block_on(send_approval_notification_if_background(
                    app.clone(),
                    sid.clone(),
                    request_id.clone(),
                    tool_name.clone(),
                    args_summary.clone(),
                ));
            }

            if is_done || is_error {
                cancel_flag.store(false, Ordering::Release);

                // Done：先持久化，再异步生成标题（不阻塞消费线程）
                let final_sid = sid.clone();

                // 提取标题生成所需数据（在锁内完成，避免长时间持锁）
                let title_task = rt.block_on(app.state::<TiangongApp>().with_state(|core_state| {
                    let _ = core_state.persist_session_and_app(&final_sid);
                    let still_running = done_event_keeps_turn_running(
                        &event,
                        core_state.has_pending_turn_for(&final_sid),
                    );
                    let snapshot = build_full_snapshot_with_status(core_state, still_running);
                    let _ = app.emit("run_snapshot", &snapshot);
                    let _ = app.emit("sessions_updated", &());

                    if still_running {
                        return Ok(None);
                    }

                    // 检查是否需要生成标题
                    if let Some(session) = core_state.sessions().iter().find(|s| s.id == final_sid)
                    {
                        let is_default =
                            session.title == "新对话" || session.title.starts_with("会话 ");
                        if is_default {
                            if let Some(input) = session
                                .messages
                                .iter()
                                .find(|m| m.role == tiangong_core::session::MessageRole::User)
                                .map(|m| m.text_content())
                            {
                                let provider_config = core_state
                                    .store
                                    .provider
                                    .models_config
                                    .to_lite_provider_config();
                                return Ok(Some((input, provider_config)));
                            }
                        }
                    }
                    Ok(None)
                }));

                // 异步生成标题（不阻塞消费线程）
                if let Ok(Some((input, provider_config))) = title_task {
                    let app_for_title = app.clone();
                    let sid_for_title = final_sid.clone();
                    let rt2 = rt.clone();
                    thread::spawn(move || {
                        let client =
                            tiangong_core::model::SingleProviderClient::new(provider_config);
                        if let Ok(t) = client.complete_lite(&input) {
                            let clean = t.trim().trim_matches('"').to_string();
                            if !clean.is_empty() {
                                let _ =
                                    rt2.block_on(app_for_title.state::<TiangongApp>().with_state(
                                        |core_state| {
                                            if let Some(s) = core_state
                                                .sessions_mut()
                                                .iter_mut()
                                                .find(|s| s.id == sid_for_title)
                                            {
                                                s.title = clean;
                                                s.updated_at = tiangong_core::session::now_text();
                                            }
                                            let _ =
                                                core_state.persist_session_and_app(&sid_for_title);
                                            let still_running =
                                                core_state.has_pending_turn_for(&sid_for_title);
                                            let snapshot = build_full_snapshot_with_status(
                                                core_state,
                                                still_running,
                                            );
                                            let _ = app_for_title.emit("run_snapshot", &snapshot);
                                            let _ = app_for_title.emit("sessions_updated", &());
                                            Ok(())
                                        },
                                    ));
                            }
                        }
                    });
                }
                // 不 break — 消费线程继续运行，等待下一轮消息的 StreamEvent
            }
        }
    });
}

/// 编辑用户消息并从该节点重新发送
///
/// 截断该消息之后的所有内容，更新消息内容，然后创建新的 core 重新执行 turn。
#[tauri::command]
pub async fn edit_and_resend(
    message_id: String,
    new_content: String,
    media: Option<Vec<tiangong_types::MediaAsset>>,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    use std::sync::mpsc;

    if let Some(ref media_vec) = media {
        if parse_context_slash_command(&new_content).is_none() && !media_vec.is_empty() {
            ensure_multimodal_enabled(&state).await?;
        }
    }

    // 1. 查找消息所在会话并验证
    let session_id = state
        .with_state(|core_state| {
            let session = core_state
                .sessions()
                .iter()
                .find(|s| s.messages.iter().any(|m| m.id == message_id))
                .ok_or_else(|| anyhow::anyhow!("消息不存在：{message_id}"))?;

            let msg_idx = session
                .messages
                .iter()
                .position(|m| m.id == message_id)
                .ok_or_else(|| anyhow::anyhow!("消息不存在"))?;
            let msg = &session.messages[msg_idx];
            if msg.role != tiangong_core::session::MessageRole::User {
                return Err(anyhow::anyhow!("只能编辑用户消息"));
            }
            if msg.compact {
                return Err(anyhow::anyhow!("该消息已被压缩或清空，无法编辑"));
            }
            if msg_idx < session.summary_up_to {
                return Err(anyhow::anyhow!("该消息已被压缩或清空，无法编辑"));
            }
            Ok(session.id.clone())
        })
        .await?;

    // 2. 取消并丢弃旧 core
    state.cancel_core(&session_id);
    state.take_core(&session_id);

    // 3. 更新消息内容、截断后续消息、持久化
    let (session_snapshot, message_media) = state
        .with_state(|core_state| {
            let session = core_state
                .sessions_mut()
                .iter_mut()
                .find(|s| s.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("会话不存在"))?;

            if let Some(ref new_media) = media {
                session.update_message_content_with_media(
                    &message_id,
                    new_content.clone(),
                    new_media.clone(),
                );
            } else {
                session.update_message_content(&message_id, new_content.clone());
            }
            session.truncate_after_message(&message_id);

            let message_media: Vec<tiangong_types::MediaAsset> = session
                .messages
                .iter()
                .find(|message| message.id == message_id)
                .map(|message| {
                    let mut assets = Vec::new();
                    for block in &message.content {
                        if let tiangong_types::message::ContentBlock::Media {
                            kind,
                            url,
                            mime_type,
                            title,
                        } = block
                        {
                            assets.push(tiangong_types::MediaAsset {
                                kind: *kind,
                                url: url.clone(),
                                mime_type: mime_type.clone(),
                                title: title.clone(),
                                capability: None,
                            });
                        }
                    }
                    assets.extend(message.media.clone());
                    assets
                })
                .unwrap_or_default();

            let mut runtime_session = session.clone();
            if runtime_session.cwd.trim().is_empty() {
                runtime_session.cwd = core_state.workspace_dir().to_string();
            }

            core_state.clear_pending_turn_for(&session_id);
            core_state.report_run_idle("正在重新发送编辑后的消息");
            core_state.persist_session_and_app(&session_id)?;

            Ok((runtime_session, message_media))
        })
        .await?;

    // 4. 创建新 core 并发送消息
    let (stream_tx, stream_rx) = mpsc::channel::<tiangong_types::SessionStreamEvent>();
    let (sid, is_new_core) = state.ensure_core(&session_id, session_snapshot, stream_tx);

    {
        let cores = state.cores.lock().map_err(|e| e.to_string())?;
        if let Some(core) = cores.get(&sid) {
            if !core.deliver(AgentInputKind::message_with_id(
                new_content.clone(),
                message_id.clone(),
                message_media.clone(),
            )) {
                return Err("会话 core 已停止，请重试".to_string());
            }
        }
    }

    if is_new_core {
        let cancel_flag = get_cancel_flag(&state, &sid)?;
        start_stream_consumer(app, stream_rx, cancel_flag);
    }

    Ok(())
}
#[tauri::command]
pub async fn cancel_turn(state: State<'_, TiangongApp>) -> Result<bool, String> {
    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
        .await?;
    state.cancel_core(&session_id);
    Ok(true)
}

async fn ensure_active_context_core(
    app: AppHandle,
    state: &State<'_, TiangongApp>,
) -> Result<String, String> {
    use std::sync::mpsc;

    state.sync_core_config_from_state().await?;

    let (session_id, session_snapshot) = state
        .with_state(|core_state| {
            let idx = core_state.ensure_active_session_index();
            let session_id = core_state.sessions()[idx].id.clone();
            let mut session_snapshot = core_state.sessions()[idx].clone();
            if session_snapshot.cwd.trim().is_empty() {
                session_snapshot.cwd = core_state.workspace_dir().to_string();
            }
            core_state.persist_session_and_app(&session_id)?;
            Ok((session_id, session_snapshot))
        })
        .await?;

    let (stream_tx, stream_rx) = mpsc::channel::<tiangong_types::SessionStreamEvent>();
    let (sid, is_new_core) = state.ensure_core(&session_id, session_snapshot, stream_tx);
    if is_new_core {
        let cancel_flag = get_cancel_flag(state, &sid)?;
        start_stream_consumer(app, stream_rx, cancel_flag);
    }
    Ok(sid)
}

#[derive(Clone, Copy)]
enum ContextSlashCommand {
    Compress,
    Reset,
}

fn parse_context_slash_command(content: &str) -> Option<ContextSlashCommand> {
    match content.trim() {
        "/compress" | "/压缩对话" => Some(ContextSlashCommand::Compress),
        "/reset" | "/清理对话" => Some(ContextSlashCommand::Reset),
        _ => None,
    }
}

async fn run_context_slash_command(
    command: ContextSlashCommand,
    app: AppHandle,
    state: &State<'_, TiangongApp>,
) -> Result<bool, String> {
    let session_id = ensure_active_context_core(app, state).await?;
    let ok = match command {
        ContextSlashCommand::Compress => state.compress_context_core(&session_id),
        ContextSlashCommand::Reset => state.reset_context_core(&session_id),
    };
    Ok(ok)
}

/// 手动触发上下文压缩
#[tauri::command]
pub async fn compress_context(
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<bool, String> {
    run_context_slash_command(ContextSlashCommand::Compress, app, &state).await
}

/// 清理上下文（重置 LLM 上下文到初始 system prompt）
#[tauri::command]
pub async fn reset_context(app: AppHandle, state: State<'_, TiangongApp>) -> Result<bool, String> {
    run_context_slash_command(ContextSlashCommand::Reset, app, &state).await
}

/// 取消当前会话中指定 Agent 的执行
#[tauri::command]
pub async fn cancel_agent(role: String, state: State<'_, TiangongApp>) -> Result<bool, String> {
    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
        .await?;
    Ok(state.cancel_agent_core(&session_id, role))
}

/// 向正在执行的 turn 追加用户消息
#[tauri::command]
pub async fn append_message(
    session_id: String,
    content: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<bool, String> {
    if session_id.trim().is_empty() {
        return Err("当前会话 ID 不能为空".to_string());
    }

    if let Some(command) = parse_context_slash_command(&content) {
        let ok = match command {
            ContextSlashCommand::Compress => state.compress_context_core(&session_id),
            ContextSlashCommand::Reset => state.reset_context_core(&session_id),
        };
        return Ok(ok);
    }

    let message_id = scru128::new().to_string();
    if !state.send_to_core_with_id(&session_id, content.clone(), Some(message_id.clone())) {
        let snapshot = state
            .with_state(|core_state| {
                core_state.report_run_idle("当前会话任务已结束，请重新发送");
                Ok(build_session_snapshot(core_state, &session_id, false))
            })
            .await?;
        let _ = app.emit("run_snapshot", &snapshot);
        return Ok(false);
    }

    let snapshot = state
        .with_state(|core_state| {
            {
                let Some(session) = core_state
                    .sessions_mut()
                    .iter_mut()
                    .find(|session| session.id == session_id)
                else {
                    return Err(anyhow::anyhow!("当前会话不存在"));
                };
                if !session.messages.iter().any(|msg| msg.id == message_id) {
                    session.append_message_with_id_and_media(
                        message_id,
                        tiangong_core::session::MessageRole::User,
                        content,
                        String::new(),
                        Vec::new(),
                    );
                }
            }

            let usage = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .map(|session| session.total_usage())
                .unwrap_or_default();
            core_state.store.session.input_draft.clear();
            core_state.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
            core_state.store.runtime.run.summary = "正在处理".to_string();
            core_state.store.runtime.run.last_session_id = Some(session_id.clone());
            core_state.store.runtime.run.last_usage = (usage.total_tokens > 0).then_some(usage);
            core_state.store.runtime.run.updated_at = tiangong_core::session::now_text();
            core_state.mark_pending_turn_for(session_id.clone());
            core_state.persist_session_and_app(&session_id)?;
            Ok(build_session_snapshot(core_state, &session_id, true))
        })
        .await?;
    let _ = app.emit("run_snapshot", &snapshot);

    Ok(true)
}

/// 响应工具审批请求
#[tauri::command]
pub async fn respond_approval(
    request_id: String,
    approved: bool,
    state: State<'_, TiangongApp>,
) -> Result<bool, String> {
    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
        .await?;
    state.respond_approval_to_core(&session_id, request_id, approved);
    Ok(true)
}

/// 获取当前信任模式
#[tauri::command]
pub async fn get_trust_mode(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| {
            let mode = core_state.active_session_trust_mode();
            Ok(serde_json::to_value(mode)
                .unwrap_or_default()
                .as_str()
                .unwrap_or("full_trust")
                .to_string())
        })
        .await
}

/// 设置信任模式
#[tauri::command]
pub async fn set_trust_mode(mode: String, state: State<'_, TiangongApp>) -> Result<(), String> {
    let trust_mode: tiangong_core::permission::TrustMode =
        serde_json::from_value(serde_json::Value::String(mode))
            .map_err(|e| format!("无效的信任模式: {e}"))?;

    state
        .with_state(|core_state| core_state.set_trust_mode(trust_mode))
        .await?;
    state.sync_core_config_from_state().await?;

    // 会话级设置只影响当前会话；其他后台会话保留自己的信任模式。
    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
        .await?;
    state.set_core_trust_mode(&session_id, trust_mode);

    Ok(())
}

#[tauri::command]
pub async fn get_default_trust_mode(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| {
            let mode = core_state.agent_config().default_trust_mode;
            Ok(serde_json::to_value(mode)
                .unwrap_or_default()
                .as_str()
                .unwrap_or("full_trust")
                .to_string())
        })
        .await
}

#[tauri::command]
pub async fn set_default_trust_mode(
    mode: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let trust_mode: tiangong_core::permission::TrustMode =
        serde_json::from_value(serde_json::Value::String(mode))
            .map_err(|e| format!("无效的默认信任模式: {e}"))?;

    state
        .with_state(|core_state| core_state.set_default_trust_mode(trust_mode))
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(())
}

#[tauri::command]
pub async fn get_custom_system_prompt(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| Ok(core_state.agent_config().custom_system_prompt.clone()))
        .await
}

#[tauri::command]
pub async fn set_custom_system_prompt(
    prompt: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    state
        .with_state(|core_state| core_state.set_custom_system_prompt(prompt))
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(())
}

#[tauri::command]
pub async fn get_reasoning_effort(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| Ok(core_state.active_session_reasoning_effort()))
        .await
}

#[tauri::command]
pub async fn set_reasoning_effort(
    effort: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let valid = ["none", "low", "medium", "high", "max"];
    if !valid.contains(&effort.as_str()) {
        return Err(format!(
            "无效的思考强度: {effort}，可选值: {}",
            valid.join("/")
        ));
    }
    state
        .with_state(|core_state| core_state.set_reasoning_effort(effort))
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(())
}

#[tauri::command]
pub async fn get_provider_balance(
    provider_name: String,
    state: State<'_, TiangongApp>,
) -> Result<serde_json::Value, String> {
    let (base_url, api_key) = state
        .with_state_read(|core_state| {
            let models = core_state.models_config();
            let provider = models
                .providers
                .get(&provider_name)
                .ok_or_else(|| anyhow::anyhow!("Provider '{provider_name}' 不存在"))?;
            let resolved_key =
                tiangong_core::models_config::ModelsConfig::resolve_api_key(&provider.api_key);
            if resolved_key.trim().is_empty() {
                return Err(anyhow::anyhow!(
                    "Provider '{provider_name}' 的 API Key 未设置"
                ));
            }
            Ok((provider.base_url.clone(), resolved_key))
        })
        .await?;

    // 余额 API 挂在域名根路径下，需去掉 base_url 中的路径部分
    // e.g. https://api.deepseek.com/anthropic → https://api.deepseek.com/user/balance
    let trimmed = base_url.trim_end_matches('/');
    let origin = if let Some(scheme_end) = trimmed.find("://") {
        let rest = &trimmed[scheme_end + 3..];
        if let Some(slash_pos) = rest.find('/') {
            trimmed[..scheme_end + 3 + slash_pos].to_string()
        } else {
            trimmed.to_string()
        }
    } else if let Some(slash_pos) = trimmed.find('/') {
        trimmed[..slash_pos].to_string()
    } else {
        trimmed.to_string()
    };
    let url = format!("{origin}/user/balance");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let response = client
        .get(&url)
        .bearer_auth(&api_key)
        .send()
        .await
        .map_err(|e| format!("请求余额失败 ({url}): {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("查询余额失败: HTTP {status}, {body}"));
    }

    response
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("解析余额响应失败: {e}"))
}

/// 获取会话成本统计
#[tauri::command]
pub async fn get_session_cost(
    session_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<serde_json::Value, String> {
    state
        .with_state_read(|core_state| {
            let sid = session_id
                .as_deref()
                .unwrap_or_else(|| core_state.active_session_id());
            let session = core_state.sessions().iter().find(|s| s.id == sid);
            match session {
                Some(s) => {
                    let cost =
                        tiangong_core::observe::build_session_cost(s.id.clone(), &s.task_records);
                    Ok(serde_json::to_value(cost).unwrap_or_default())
                }
                None => Ok(serde_json::json!({})),
            }
        })
        .await
}

/// 获取当前活跃的 Worker 列表
#[tauri::command]
pub async fn list_workers(state: State<'_, TiangongApp>) -> Result<Vec<serde_json::Value>, String> {
    state
        .with_state_read(|core_state| Ok(core_state.list_active_workers()))
        .await
}

/// 获取后台任务列表
#[tauri::command]
pub async fn get_background_tasks() -> Result<Vec<serde_json::Value>, String> {
    let reg = tiangong_core::tool::background_task::task_registry();
    let mut guard = reg.lock().map_err(|e| e.to_string())?;
    let tasks = guard.list();
    tasks
        .into_iter()
        .map(|t| serde_json::to_value(t).map_err(|e| e.to_string()))
        .collect()
}

/// 取消后台任务
#[tauri::command]
pub async fn cancel_background_task(task_id: String) -> Result<(), String> {
    let reg = tiangong_core::tool::background_task::task_registry();
    let mut guard = reg.lock().map_err(|e| e.to_string())?;
    guard.cancel(&task_id);
    Ok(())
}

/// 语音合成：将文本转换为音频，返回 base64 编码的音频数据
#[tauri::command]
pub async fn synthesize_speech(
    text: String,
    state: State<'_, TiangongApp>,
) -> Result<SpeechResult, String> {
    let models_config = state
        .with_state_read(|core_state| Ok(core_state.models_config().clone()))
        .await?;
    let output = tiangong_core::media::synthesize_speech(
        &models_config,
        text,
        None,
        None,
        Some("mp3".to_string()),
    )
    .await
    .map_err(|e| e.to_string())?;
    let resp = output.response;

    // 将音频保存到临时文件，通过 asset 协议播放
    let media_dir = user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("media");
    std::fs::create_dir_all(&media_dir).map_err(|e| format!("创建媒体目录失败：{e}"))?;

    let ext = match resp.mime_type.as_str() {
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/opus" => "opus",
        "audio/aac" => "aac",
        "audio/flac" => "flac",
        _ => "mp3",
    };
    let file_name = format!("tts_{}.{}", scru128::new(), ext);
    let file_path = media_dir.join(&file_name);
    std::fs::write(&file_path, &resp.audio).map_err(|e| format!("音频文件写入失败：{e}"))?;

    Ok(SpeechResult {
        file_path: file_path.to_string_lossy().to_string(),
        mime_type: resp.mime_type,
    })
}

/// 检查 TTS 能力是否已配置
#[tauri::command]
pub async fn has_tts_capability(state: State<'_, TiangongApp>) -> Result<bool, String> {
    has_model_capability("tts".to_string(), state).await
}

/// 检查 STT 能力是否已配置
#[tauri::command]
pub async fn has_stt_capability(state: State<'_, TiangongApp>) -> Result<bool, String> {
    has_model_capability("stt".to_string(), state).await
}

/// 统一的能力可用性查询（基于配置快速检测）
#[tauri::command]
pub async fn has_model_capability(
    capability: String,
    state: State<'_, TiangongApp>,
) -> Result<bool, String> {
    let capability = parse_model_capability(&capability)?;
    state
        .with_state_read(|core_state| Ok(has_capability_in_state(core_state, capability)))
        .await
}

/// 获取所有能力的当前配置状态
#[tauri::command]
pub async fn get_available_capabilities(
    state: State<'_, TiangongApp>,
) -> Result<Vec<CapabilityAvailabilityInfo>, String> {
    use tiangong_core::models_config::ModelCapability;

    state
        .with_state_read(|core_state| {
            Ok(ModelCapability::all()
                .iter()
                .map(|capability| CapabilityAvailabilityInfo {
                    key: capability.key().to_string(),
                    display_name: capability.display_name().to_string(),
                    enabled: has_capability_in_state(core_state, *capability),
                    routed_model: core_state
                        .models_config()
                        .routed_model(*capability)
                        .map(str::to_string),
                })
                .collect())
        })
        .await
}

/// 语音识别：将音频数据转录为文本，同时保存音频文件
#[tauri::command]
pub async fn transcribe_speech(
    audio_base64: String,
    mime_type: String,
    state: State<'_, TiangongApp>,
) -> Result<TranscribeResult, String> {
    let models_config = state
        .with_state_read(|core_state| Ok(core_state.models_config().clone()))
        .await?;

    // 解码 base64 音频数据
    use base64::Engine;
    let audio = base64::engine::general_purpose::STANDARD
        .decode(&audio_base64)
        .map_err(|e| format!("音频数据解码失败：{e}"))?;

    // 保存音频文件
    let media_dir = user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("media");
    std::fs::create_dir_all(&media_dir).map_err(|e| format!("创建媒体目录失败：{e}"))?;

    let ext = match mime_type.as_str() {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mp3" | "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/webm" => "webm",
        _ => "wav",
    };
    let file_name = format!("stt_{}.{}", scru128::new(), ext);
    let file_path = media_dir.join(&file_name);
    std::fs::write(&file_path, &audio).map_err(|e| format!("音频文件保存失败：{e}"))?;

    let audio_path = file_path.to_string_lossy().to_string();
    let output = tiangong_core::media::transcribe_audio(&models_config, audio, mime_type, None)
        .await
        .map_err(|e| e.to_string())?;

    Ok(TranscribeResult {
        text: output.response.text,
        audio_path,
        duration: output.response.duration,
    })
}

/// 获取 TTS 可用音色列表
#[tauri::command]
pub async fn list_tts_voices(
    state: State<'_, TiangongApp>,
) -> Result<Vec<serde_json::Value>, String> {
    let models_config = state
        .with_state_read(|core_state| Ok(core_state.models_config().clone()))
        .await?;
    let voices = tiangong_core::media::list_tts_voices(&models_config)
        .await
        .map_err(|e| e.to_string())?;

    Ok(voices
        .into_iter()
        .map(|v| {
            serde_json::json!({
                "id": v.id,
                "name": v.name,
                "gender": v.gender,
            })
        })
        .collect())
}

/// 播放本地音频文件（使用系统原生播放器）
#[tauri::command]
pub async fn play_audio_file(file_path: String) -> Result<(), String> {
    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Err(format!("音频文件不存在：{file_path}"));
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = tokio::process::Command::new("afplay");
        command.arg(&file_path);
        configure_no_window(&mut command);
        command
            .output()
            .await
            .map_err(|e| format!("播放失败：{e}"))?;
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = tokio::process::Command::new("powershell");
        command.args([
            "-c",
            &format!("(New-Object Media.SoundPlayer '{}').PlaySync()", file_path),
        ]);
        configure_no_window(&mut command);
        command
            .output()
            .await
            .map_err(|e| format!("播放失败：{e}"))?;
    }

    #[cfg(target_os = "linux")]
    {
        let mut command = tokio::process::Command::new("aplay");
        command.arg(&file_path);
        configure_no_window(&mut command);
        command
            .output()
            .await
            .map_err(|e| format!("播放失败：{e}"))?;
    }

    Ok(())
}

/// 停止当前正在播放的音频
#[tauri::command]
pub async fn stop_audio() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = tokio::process::Command::new("killall")
            .arg("afplay")
            .output()
            .await;
    }
    Ok(())
}

/// 获取 @提及补全候选列表（已启用的 Skill 和 MCP 服务器）
#[tauri::command]
pub async fn get_mention_candidates(
    state: State<'_, TiangongApp>,
) -> Result<Vec<MentionCandidate>, String> {
    state
        .with_state_read(|core_state| {
            let mut candidates = Vec::new();

            // 已启用的 Skill
            for skill in core_state.installed_skills() {
                if skill.enabled {
                    candidates.push(MentionCandidate {
                        value: format!("@skill:{}", skill.id),
                        label: skill.name.clone(),
                        kind: "skill".to_string(),
                        hint: if skill.description.is_empty() {
                            format!("v{}", skill.version)
                        } else {
                            skill.description.clone()
                        },
                    });
                }
            }

            // 已启用的 MCP 服务器
            let active_tools = tiangong_core::mcp::cached_active_tools();
            for server in core_state.mcp_servers() {
                if server.enabled {
                    let tool_count = active_tools
                        .iter()
                        .find(|(name, _)| name == &server.name)
                        .map(|(_, tools)| tools.len())
                        .unwrap_or(0);
                    candidates.push(MentionCandidate {
                        value: format!("@mcp:{}", server.name),
                        label: server.name.clone(),
                        kind: "mcp".to_string(),
                        hint: format!("{} 工具", tool_count),
                    });
                }
            }

            Ok(candidates)
        })
        .await
}

/// 获取运行状态快照
#[tauri::command]
pub async fn get_run_snapshot(state: State<'_, TiangongApp>) -> Result<RunSnapshotView, String> {
    let active_id = state
        .with_state_read(|s| Ok(s.active_session_id().to_string()))
        .await?;
    let is_exec = state
        .with_state_read(|s| {
            let snapshot = s.run_snapshot();
            Ok(s.has_pending_turn_for(&active_id)
                || (snapshot.last_session_id.as_deref() == Some(active_id.as_str())
                    && snapshot.status != tiangong_types::RunStatus::Idle))
        })
        .await?;
    state
        .with_state_read(|core_state| Ok(build_full_snapshot_with_status(core_state, is_exec)))
        .await
}

/// 获取输入草稿
#[tauri::command]
pub async fn get_input_draft(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| Ok(core_state.input_draft().to_string()))
        .await
}

/// 设置输入草稿
#[tauri::command]
pub async fn set_input_draft(content: String, state: State<'_, TiangongApp>) -> Result<(), String> {
    state
        .with_state(|core_state| {
            core_state.update_draft(content);
            Ok(())
        })
        .await
}

/// 获取活动会话的工作目录
#[tauri::command]
pub async fn get_session_cwd(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| Ok(core_state.active_session_cwd().to_string()))
        .await
}

/// 获取 Desktop 工作空间目录
#[tauri::command]
pub async fn get_workspace_dir(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| Ok(core_state.workspace_dir().to_string()))
        .await
}

/// 设置 Desktop 工作空间目录
#[tauri::command]
pub async fn set_workspace_dir(
    app: tauri::AppHandle,
    workspace_dir: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let path = std::path::Path::new(&workspace_dir);
    if !path.is_dir() {
        return Err(format!("路径不存在或不是目录：{workspace_dir}"));
    }
    state
        .with_state(|core_state| core_state.update_workspace_dir(workspace_dir.clone()))
        .await?;

    // 同步终端：更新终端默认 cwd（后续懒创建的 PTY），并对所有存活 PTY
    // 发送 cd 使已打开的终端进入新 workspace。
    tiangong_plugin_terminal::sync_workspace_cwd(&app, &workspace_dir);

    // 同步活跃 core 的 cwd（仅限无对话的 Inherit 会话）
    let active_session_id = state
        .with_state_read(|core_state| {
            Ok(core_state
                .sessions()
                .iter()
                .find(|s| s.id == core_state.active_session_id())
                .filter(|s| s.cwd_mode == tiangong_core::session::SessionCwdMode::Inherit)
                .filter(|s| !s.has_user_messages())
                .map(|s| s.id.clone()))
        })
        .await?;
    if let Some(sid) = &active_session_id {
        let cores = state.cores.lock().map_err(|e| e.to_string())?;
        if let Some(core) = cores.get(sid) {
            let _ = core.deliver(AgentInputKind::update_cwd(workspace_dir.clone()));
        }
    }

    Ok(())
}

/// 设置活动会话的工作目录
#[tauri::command]
pub async fn set_session_cwd(cwd: String, state: State<'_, TiangongApp>) -> Result<(), String> {
    let path = std::path::Path::new(&cwd);
    if !path.is_dir() {
        return Err(format!("路径不存在或不是目录：{cwd}"));
    }
    state
        .with_state(|core_state| core_state.update_active_session_cwd(cwd.clone()))
        .await?;

    // 同步活跃 core 的 cwd
    let active_session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
        .await?;
    {
        let cores = state.cores.lock().map_err(|e| e.to_string())?;
        if let Some(core) = cores.get(&active_session_id) {
            let _ = core.deliver(AgentInputKind::update_cwd(cwd.clone()));
        }
    }

    Ok(())
}

// ============================================================================
// MCP 管理
// ============================================================================

/// 获取 MCP 服务器列表
#[tauri::command]
pub async fn get_mcp_servers(state: State<'_, TiangongApp>) -> Result<Vec<McpServerView>, String> {
    state
        .with_state_read(|core_state| {
            Ok(core_state
                .mcp_servers()
                .iter()
                .map(McpServerView::from_core)
                .collect())
        })
        .await
}

/// 获取 MCP 服务器健康状态
#[tauri::command]
pub async fn get_mcp_health() -> Result<Vec<serde_json::Value>, String> {
    let statuses = tiangong_core::mcp::mcp_server_health_statuses();
    statuses
        .into_iter()
        .map(|s| serde_json::to_value(s).map_err(|e| e.to_string()))
        .collect()
}

/// 注册 MCP 服务器
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn register_mcp_server(
    name: String,
    command: String,
    args: Vec<String>,
    transport: Option<String>,
    endpoint: Option<String>,
    auth_header: Option<String>,
    headers: Option<std::collections::HashMap<String, String>>,
    env: Option<std::collections::HashMap<String, String>>,
    cwd: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    use tiangong_core::agent_config::McpTransportMode;
    use tiangong_core::app_state::RegisterMcpServerOptions;
    use tiangong_core::app_state::RegisterMcpServerRequest;

    let transport = match transport
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => match value.to_ascii_lowercase().as_str() {
            "auto" => Some(McpTransportMode::Auto),
            "stdio" => Some(McpTransportMode::Stdio),
            "http" | "sse" | "streamablehttp" | "streamable_http" | "streamable-http" => {
                Some(McpTransportMode::Http)
            }
            other => {
                return Err(format!(
                    "不支持的 MCP transport：{other}，支持 auto/stdio/http/sse"
                ));
            }
        },
        None => None,
    };

    let message = state
        .with_state(|core_state| {
            let header_vec = headers.unwrap_or_default().into_iter().collect();
            let env_vec = env.unwrap_or_default().into_iter().collect();

            let request = RegisterMcpServerRequest {
                name: name.clone(),
                command,
                args,
                tags: vec![],
                enabled: true,
                options: RegisterMcpServerOptions {
                    transport,
                    endpoint,
                    auth_header,
                    headers: header_vec,
                    env: env_vec,
                    cwd,
                },
            };
            core_state.register_mcp_server(request)
        })
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

/// 移除 MCP 服务器
#[tauri::command]
pub async fn remove_mcp_server(
    name: String,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let message = state
        .with_state(|core_state| core_state.remove_mcp_server(&name))
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

/// 设置 MCP 服务器启用状态
#[tauri::command]
pub async fn set_mcp_server_enabled(
    name: String,
    enabled: bool,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let message = state
        .with_state(|core_state| core_state.set_mcp_server_enabled(&name, enabled))
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

// ============================================================================
// Skill 管理
// ============================================================================

/// 获取已安装的 Skill 列表
#[tauri::command]
pub async fn get_skills(state: State<'_, TiangongApp>) -> Result<Vec<SkillView>, String> {
    state
        .with_state_read(|core_state| {
            Ok(core_state
                .installed_skills()
                .iter()
                .map(SkillView::from_core)
                .collect())
        })
        .await
}

/// 刷新 Skill 注册表（重扫 skills/<id>/）
#[tauri::command]
pub async fn refresh_skills(state: State<'_, TiangongApp>) -> Result<String, String> {
    let message = state
        .with_state(|core_state| core_state.refresh_skills())
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

/// 检测或清理孤儿 Skill 托管 MCP 配置
#[tauri::command]
pub async fn gc_skills(apply: bool, state: State<'_, TiangongApp>) -> Result<String, String> {
    let message = state
        .with_state(|core_state| core_state.gc_skills(apply))
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

/// 获取 Skill 完整详情（按需读取 SKILL.md）
#[tauri::command]
pub async fn get_skill_detail(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<SkillDetailView, String> {
    state
        .with_state_read(|core_state| {
            let detail = core_state.get_skill_detail(&id)?;
            Ok(SkillDetailView::from_core(&detail))
        })
        .await
}

/// 检查 Skill 安装需求（返回需要配置的环境变量列表）
#[tauri::command]
pub async fn inspect_skill(
    path: String,
    state: State<'_, TiangongApp>,
) -> Result<SkillInspection, String> {
    state
        .with_state_read(|core_state| {
            let inspection = core_state.inspect_skill_install_requirements(&path, true)?;
            Ok(SkillInspection {
                env_vars: inspection.env_vars,
                missing_env_vars: inspection.missing_env_vars,
                dependencies: inspection.dependencies,
            })
        })
        .await
}

/// 安装 Skill（支持传入环境变量配置）
#[tauri::command]
pub async fn install_skill(
    path: String,
    env_values: Option<std::collections::HashMap<String, String>>,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let message = state
        .with_state(|core_state| {
            let env: Vec<(String, String)> = env_values
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, v)| !v.trim().is_empty())
                .collect();
            core_state.install_local_skill_with_options_and_inputs(&path, true, true, &env)
        })
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

/// 移除 Skill
#[tauri::command]
pub async fn remove_skill(id: String, state: State<'_, TiangongApp>) -> Result<String, String> {
    let message = state
        .with_state(|core_state| core_state.remove_skill(&id))
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

/// 获取 Skill 的环境变量（合并 skill.toml 声明的 requires.env + .env.local 已有值）
#[tauri::command]
pub async fn get_skill_env(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<std::collections::HashMap<String, String>, String> {
    state
        .with_state_read(|core_state| {
            let skill = core_state
                .installed_skills()
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;

            let skill_dir = std::path::Path::new(&skill.source.value);
            let mut env = std::collections::HashMap::new();

            // 1. 从 skill.toml 的 requires.env 读取声明的 key（值为空）
            let toml_path = skill_dir.join("skill.toml");
            if let Ok(raw) = std::fs::read_to_string(&toml_path) {
                #[derive(serde::Deserialize, Default)]
                struct T {
                    #[serde(default)]
                    requires: R,
                }
                #[derive(serde::Deserialize, Default)]
                struct R {
                    #[serde(default)]
                    env: Vec<String>,
                }
                if let Ok(parsed) = toml::from_str::<T>(&raw) {
                    for key in parsed.requires.env {
                        env.insert(key, String::new());
                    }
                }
            }

            // 2. 从 .env.local 读取已有值（覆盖空值）
            let env_path = skill_dir.join(".env.local");
            if let Ok(content) = std::fs::read_to_string(&env_path) {
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some((k, v)) = line.split_once('=') {
                        env.insert(k.trim().to_string(), v.trim().to_string());
                    }
                }
            }

            Ok(env)
        })
        .await
}

/// 设置 Skill 的环境变量
#[tauri::command]
pub async fn set_skill_env(
    id: String,
    env: std::collections::HashMap<String, String>,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    state
        .with_state_read(|core_state| {
            let skill = core_state
                .installed_skills()
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;
            let env_path = std::path::Path::new(&skill.source.value).join(".env.local");
            let lines: Vec<String> = env
                .iter()
                .filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty())
                .map(|(k, v)| format!("{}={}", k.trim(), v.trim()))
                .collect();
            if lines.is_empty() {
                let _ = std::fs::remove_file(&env_path);
            } else {
                std::fs::write(&env_path, format!("{}\n", lines.join("\n")))
                    .map_err(|e| anyhow::anyhow!("写入 .env.local 失败：{e}"))?;
            }
            Ok(())
        })
        .await
}

/// 设置 Skill 启用状态
#[tauri::command]
pub async fn set_skill_enabled(
    id: String,
    enabled: bool,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let message = state
        .with_state(|core_state| core_state.set_skill_enabled(&id, enabled))
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

// ============================================================================
// Server 管理
// ============================================================================

/// 获取 Server 配置
#[tauri::command]
pub fn get_server_config(state: State<'_, TiangongApp>) -> Result<ServerConfigView, String> {
    let config = tiangong_server::config::load_server_config();
    let running = state.is_embedded_server_running() || is_server_running(&config);
    let auth_token_masked = config.masked_auth_token();
    Ok(ServerConfigView {
        host: config.host,
        port: config.port,
        auth_token_masked,
        running,
    })
}

/// 设置 Server 配置
#[tauri::command]
pub async fn set_server_config(
    host: String,
    port: u16,
    auth_token: Option<String>,
) -> Result<String, String> {
    let current = tiangong_server::config::load_server_config();
    let config = tiangong_server::config::ServerConfig {
        host,
        port,
        auth_token: auth_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
            .or(current.auth_token),
        enabled: current.enabled,
    };
    tiangong_server::config::save_server_config(&config).map_err(|e| e.to_string())?;
    Ok("Server 配置已保存".to_string())
}

/// 启动嵌入式 Server（Desktop 模式下 Server 运行在 app 进程内）
#[tauri::command]
pub async fn start_server(state: State<'_, TiangongApp>) -> Result<String, String> {
    let config = tiangong_server::config::load_server_config();

    // 优先检查嵌入式 server 是否已运行
    if state.is_embedded_server_running() {
        return Ok("Server 已在运行（嵌入式）".to_string());
    }

    // 检查是否有外部 server 进程占用端口
    if server_health_check(&config) {
        return Ok("Server 已在运行（外部进程）".to_string());
    }

    state.start_embedded_server(&config.host, config.port, config.auth_token.clone())?;

    // 等待健康检查通过
    if let Err(err) = wait_for_server_health(&config) {
        let _ = state.stop_embedded_server();
        return Err(err);
    }

    // 持久化 enabled 标记，重启后自动拉起
    let mut config = config;
    config.enabled = true;
    let _ = tiangong_server::config::save_server_config(&config);

    Ok(format!("Server 已启动：{}:{}", config.host, config.port))
}

/// 停止 Server
#[tauri::command]
pub async fn stop_server(state: State<'_, TiangongApp>) -> Result<String, String> {
    // 优先停止嵌入式 server
    if state.is_embedded_server_running() {
        state.stop_embedded_server()?;

        // 持久化 enabled 标记
        let mut config = tiangong_server::config::load_server_config();
        config.enabled = false;
        let _ = tiangong_server::config::save_server_config(&config);

        return Ok("Server 已停止".to_string());
    }

    // 兜底：检查是否有外部 server 进程
    let config = tiangong_server::config::load_server_config();
    let running_by_health = server_health_check(&config);
    let running_by_pid = server_pid_alive();
    if !running_by_health && !running_by_pid {
        cleanup_dead_server_pid();
        return Ok("Server 未运行".to_string());
    }
    if running_by_health && !running_by_pid {
        cleanup_dead_server_pid();
        return Err("Server 正在运行，但后台 PID 文件缺失或已失效，无法通过应用安全停止。请先手动关闭占用端口的 Server。".to_string());
    }
    tiangong_server::stop_daemon().map_err(|e| e.to_string())?;
    wait_for_server_stop(&config)?;

    // 持久化 enabled 标记
    let mut config = config;
    config.enabled = false;
    let _ = tiangong_server::config::save_server_config(&config);

    Ok("Server 已停止".to_string())
}

/// 检查 Server 是否在运行：优先访问健康检查，PID 仅作为兜底。
pub fn is_server_running(config: &tiangong_server::config::ServerConfig) -> bool {
    if server_health_check(config) {
        return true;
    }

    cleanup_dead_server_pid();
    false
}

fn server_pid_path() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("server.pid")
}

fn server_pid_alive() -> bool {
    let pid_path = server_pid_path();
    if !pid_path.exists() {
        return false;
    }
    match std::fs::read_to_string(&pid_path) {
        Ok(pid_str) => {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                // 检查进程是否存在
                #[cfg(unix)]
                {
                    use std::process::Command;
                    Command::new("kill")
                        .arg("-0")
                        .arg(pid.to_string())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    let _ = pid;
                    false
                }
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

fn cleanup_dead_server_pid() {
    let pid_path = server_pid_path();
    if pid_path.exists() && !server_pid_alive() {
        let _ = std::fs::remove_file(pid_path);
    }
}

fn wait_for_server_health(config: &tiangong_server::config::ServerConfig) -> Result<(), String> {
    for _ in 0..30 {
        if server_health_check(config) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("Server 已启动但健康检查未通过".to_string())
}

fn wait_for_server_stop(config: &tiangong_server::config::ServerConfig) -> Result<(), String> {
    for _ in 0..30 {
        if !server_health_check(config) {
            cleanup_dead_server_pid();
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("已发送停止信号，但 Server 仍在响应健康检查".to_string())
}

fn server_health_check(config: &tiangong_server::config::ServerConfig) -> bool {
    let host = connect_host(&config.host);
    let Ok(mut addrs) = (host.as_str(), config.port).to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(150)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(150)));
    let request = format!(
        "GET /api/v1/health HTTP/1.1\r\nHost: {host}:{}\r\nConnection: close\r\n\r\n",
        config.port
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = [0_u8; 1024];
    let Ok(len) = stream.read(&mut response) else {
        return false;
    };
    let response = String::from_utf8_lossy(&response[..len]);
    response.starts_with("HTTP/") && response.contains(" 200 ") && response.contains("status")
}

fn connect_host(host: &str) -> String {
    match host.trim() {
        "" | "0.0.0.0" | "::" => "127.0.0.1".to_string(),
        value => value.to_string(),
    }
}

/// 获取用户 home 目录
fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| v != std::ffi::OsStr::new(""))
    {
        return Some(PathBuf::from(profile));
    }
    None
}

// ============================================================================
// 模型配置（Provider + Model + Routing 三层架构）
// ============================================================================

/// 获取模型配置
#[tauri::command]
pub async fn get_models_config(state: State<'_, TiangongApp>) -> Result<ModelsConfigView, String> {
    state
        .with_state_read(|core_state| Ok(ModelsConfigView::from_core(core_state.models_config())))
        .await
}

/// 设置模型配置
#[tauri::command]
pub async fn set_models_config(
    config: ModelsConfigView,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    state
        .with_state(|core_state| {
            let core_config = config.to_core();
            core_state.save_models_config(core_config)
        })
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(())
}

/// 获取 Memory 独立模型配置
#[tauri::command]
pub async fn get_memory_config(state: State<'_, TiangongApp>) -> Result<MemoryConfigView, String> {
    let config = tiangong_core::core::load_memory_config();
    state
        .with_state_read(|core_state| {
            Ok(MemoryConfigView::from_memory(
                &config,
                core_state.models_config(),
            ))
        })
        .await
}

/// 设置 Memory 独立模型配置
#[tauri::command]
pub async fn set_memory_config(
    config: MemoryConfigView,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let memory_config = state
        .with_state_read(|core_state| {
            config
                .to_memory(core_state.models_config())
                .map_err(anyhow::Error::msg)
        })
        .await?;
    tiangong_core::core::save_memory_config(memory_config).map_err(|err| err.to_string())?;
    state.sync_core_config_from_state().await?;
    Ok(())
}

/// 列出全部记忆节点。
#[tauri::command]
pub async fn list_memory_nodes(
    query: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_memory::MemoryNode>, String> {
    tiangong_core::core::list_memory_nodes_for_gui(&state.config, query, status, limit, offset)
        .await
        .map_err(|err| err.to_string())
}

/// 统计全部记忆节点真实总数。
#[tauri::command]
pub async fn count_memory_nodes(
    query: Option<String>,
    status: Option<String>,
    created_after: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<usize, String> {
    tiangong_core::core::count_memory_nodes_for_gui(&state.config, query, status, created_after)
        .await
        .map_err(|err| err.to_string())
}

/// 手动新增或调整一条记忆。
#[tauri::command]
pub async fn upsert_manual_memory(
    draft: tiangong_memory::ManualMemoryDraft,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_memory::MemoryNode, String> {
    if draft.title.trim().is_empty() {
        return Err("记忆标题不能为空".to_string());
    }
    if draft.summary.trim().is_empty() {
        return Err("记忆内容不能为空".to_string());
    }
    tiangong_core::core::upsert_manual_memory_for_gui(&state.config, draft)
        .await
        .map_err(|err| err.to_string())
}

/// 归档或恢复记忆节点。
#[tauri::command]
pub async fn set_memory_node_status(
    node_id: String,
    status: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    tiangong_core::core::set_memory_node_status_for_gui(&state.config, node_id, status)
        .await
        .map_err(|err| err.to_string())
}

/// 列出指定记忆节点的图关系。
#[tauri::command]
pub async fn list_memory_relations(
    node_id: String,
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_memory::MemoryRelation>, String> {
    tiangong_core::core::list_memory_relations_for_gui(&state.config, node_id)
        .await
        .map_err(|err| err.to_string())
}

/// 批量列出多个记忆节点的关联关系（去重，修复 N+1 性能问题）。
#[tauri::command]
pub async fn list_memory_relations_batch(
    node_ids: Vec<String>,
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_memory::MemoryRelation>, String> {
    tiangong_core::core::list_memory_relations_batch_for_gui(&state.config, node_ids)
        .await
        .map_err(|err| err.to_string())
}

/// 新增或调整记忆图关系。
#[tauri::command]
pub async fn upsert_memory_relation(
    draft: tiangong_memory::MemoryRelationDraft,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_memory::MemoryRelation, String> {
    tiangong_core::core::upsert_memory_relation_for_gui(&state.config, draft)
        .await
        .map_err(|err| err.to_string())
}

/// 删除记忆图关系。
#[tauri::command]
pub async fn delete_memory_relation(
    relation_id: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    tiangong_core::core::delete_memory_relation_for_gui(&state.config, relation_id)
        .await
        .map_err(|err| err.to_string())
}

/// 手动测试记忆召回，不写入会话消息链。
#[tauri::command]
pub async fn test_memory_recall(
    query: String,
    limit: Option<usize>,
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_memory::RecallHit>, String> {
    tiangong_core::core::test_memory_recall_for_gui(&state.config, query, limit)
        .await
        .map_err(|err| err.to_string())
}

// ── 索引管理 ──

/// 列出所有 Workspace 索引
#[tauri::command]
pub async fn list_workspace_indexes() -> Result<Vec<tiangong_core::core::WorkspaceIndexInfo>, String>
{
    tiangong_core::core::list_workspace_indexes_for_gui().map_err(|err| err.to_string())
}

/// 删除指定 Workspace 索引
#[tauri::command]
pub async fn delete_workspace_index(workspace_id: String) -> Result<(), String> {
    tiangong_core::core::delete_workspace_index_for_gui(&workspace_id)
        .map_err(|err| err.to_string())
}

/// 重建指定路径的 Workspace 索引
#[tauri::command]
pub async fn rebuild_workspace_index(root: String) -> Result<usize, String> {
    let root = std::path::PathBuf::from(&root);
    tiangong_core::core::rebuild_workspace_index_for_gui(&root).map_err(|err| err.to_string())
}

/// 获取所有可用的模型能力列表
#[tauri::command]
pub async fn get_model_capabilities() -> Result<Vec<ModelCapabilityInfo>, String> {
    use tiangong_core::models_config::ModelCapability;

    let caps = ModelCapability::all()
        .iter()
        .map(|c| {
            let key = serde_json::to_value(c).unwrap_or_default();
            ModelCapabilityInfo {
                key: key.as_str().unwrap_or_default().to_string(),
                display_name: c.display_name().to_string(),
            }
        })
        .collect();
    Ok(caps)
}

/// 获取模型列表
#[tauri::command]
pub async fn get_model_list(state: State<'_, TiangongApp>) -> Result<Vec<String>, String> {
    state
        .with_state_read(|core_state| Ok(core_state.model_list().to_vec()))
        .await
}

/// 根据 provider 配置获取该 provider 的可用模型列表
#[tauri::command]
pub async fn fetch_provider_models(
    base_url: String,
    api_key: String,
    timeout_ms: Option<u64>,
    protocol: Option<String>,
) -> Result<Vec<String>, String> {
    use tiangong_core::model::{ModelProviderConfig, ProviderProtocol, SingleProviderClient};
    use tiangong_core::models_config::ModelsConfig;

    let resolved_key = ModelsConfig::resolve_api_key(&api_key);
    let config = ModelProviderConfig {
        api_auth_token: resolved_key,
        api_base_url: base_url,
        api_timeout_ms: timeout_ms.unwrap_or(60_000).to_string(),
        api_protocol: protocol
            .as_deref()
            .and_then(|value| value.parse::<ProviderProtocol>().ok())
            .unwrap_or_default(),
        api_model: String::new(),
        api_lite_model: String::new(),
    };
    SingleProviderClient::list_models_async(&config)
        .await
        .map_err(|e| e.to_string())
}

fn embedding_probe_urls(base_url: &str) -> Result<Vec<String>, String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err("Base URL 不能为空".to_string());
    }

    let cleaned = trimmed.trim_end_matches('/');
    let cleaned = cleaned.strip_suffix("/chat/completions").unwrap_or(cleaned);
    let cleaned = cleaned.strip_suffix("/embeddings").unwrap_or(cleaned);
    let primary = format!("{cleaned}/embeddings");

    if cleaned.ends_with("/v1") {
        return Ok(vec![primary]);
    }

    Ok(vec![primary, format!("{cleaned}/v1/embeddings")])
}

/// 探测 OpenAI 兼容 Embedding 接口返回的向量维度
#[tauri::command]
pub async fn probe_embedding_dimension(
    base_url: String,
    api_key: String,
    model: String,
    timeout_ms: Option<u64>,
    protocol: Option<String>,
) -> Result<usize, String> {
    use tiangong_core::model::ProviderProtocol;
    use tiangong_core::models_config::ModelsConfig;

    let protocol = protocol
        .as_deref()
        .unwrap_or_default()
        .parse::<ProviderProtocol>()
        .map_err(|err| err.to_string())?;
    if !matches!(protocol, ProviderProtocol::OpenAiChatCompletions) {
        return Err("Embedding 维度探测仅支持 OpenAI 兼容协议".to_string());
    }

    let model = model.trim();
    if model.is_empty() {
        return Err("模型名称不能为空".to_string());
    }

    let urls = embedding_probe_urls(&base_url)?;
    let api_key = ModelsConfig::resolve_api_key(&api_key);
    let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(60_000));
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| format!("创建 HTTP 客户端失败：{err}"))?;
    let payload = serde_json::json!({
        "model": model,
        "input": "dimension probe",
        "encoding_format": "float",
    });

    let mut last_error = None;
    for url in urls {
        let mut request = client.post(&url).json(&payload);
        if !api_key.trim().is_empty() {
            request = request.bearer_auth(&api_key);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                last_error = Some(format!("请求 Embedding 接口失败：{url}，{err}"));
                continue;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            last_error = Some(format!(
                "Embedding 接口返回错误：HTTP {status}，响应：{body}"
            ));
            continue;
        }

        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|err| format!("解析 Embedding 响应失败：{err}"))?;
        let embedding = value
            .pointer("/data/0/embedding")
            .and_then(|value| value.as_array())
            .ok_or_else(|| "Embedding 响应中缺少 data[0].embedding".to_string())?;
        if embedding.is_empty() {
            return Err("Embedding 响应向量为空".to_string());
        }
        return Ok(embedding.len());
    }

    Err(last_error.unwrap_or_else(|| "无法请求 Embedding 接口".to_string()))
}

// ── 定时任务管理 ──────────────────────────────────────────────

#[tauri::command]
pub async fn job_list() -> Result<Vec<serde_json::Value>, String> {
    let store = tiangong_core::scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
    let jobs = store.list_jobs().map_err(|e| e.to_string())?;
    Ok(jobs
        .into_iter()
        .map(|j| serde_json::to_value(j).unwrap())
        .collect())
}

#[tauri::command]
pub async fn job_create(
    name: String,
    description: String,
    schedule: String,
    session_id: Option<String>,
    payload: String,
    enabled: Option<bool>,
) -> Result<serde_json::Value, String> {
    let store = tiangong_core::scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
    let now = chrono::Local::now().naive_local().to_string();
    let job = tiangong_core::scheduler::model::Job {
        id: scru128::new().to_string(),
        name,
        description,
        trigger_type: tiangong_core::scheduler::model::TriggerType::Cron,
        schedule: Some(schedule),
        session_id,
        payload,
        enabled: enabled.unwrap_or(true),
        created_at: now.clone(),
        updated_at: now,
    };
    store.insert_job(&job).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(job).unwrap())
}

#[tauri::command]
pub async fn job_update(
    id: String,
    name: Option<String>,
    description: Option<String>,
    schedule: Option<String>,
    session_id: Option<String>,
    payload: Option<String>,
    enabled: Option<bool>,
) -> Result<serde_json::Value, String> {
    let store = tiangong_core::scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
    let req = tiangong_core::scheduler::model::UpdateJobRequest {
        name,
        description,
        schedule,
        session_id,
        payload,
        enabled,
    };
    let updated = store.update_job(&id, &req).map_err(|e| e.to_string())?;
    if !updated {
        return Err(format!("定时任务 '{id}' 不存在"));
    }
    let job = store.get_job(&id).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(job).unwrap())
}

#[tauri::command]
pub async fn job_delete(id: String) -> Result<(), String> {
    let store = tiangong_core::scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
    let deleted = store.delete_job(&id).map_err(|e| e.to_string())?;
    if !deleted {
        return Err(format!("定时任务 '{id}' 不存在"));
    }
    Ok(())
}

#[tauri::command]
pub async fn job_trigger(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<serde_json::Value, String> {
    let store = tiangong_core::scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
    let job = store
        .get_job(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("定时任务 '{id}' 不存在"))?;

    let ctx = state.create_scheduler_context();
    let job_clone = job.clone();
    tokio::spawn(async move {
        tiangong_scheduler::executor::execute_job(ctx, job_clone).await;
    });

    Ok(serde_json::json!({
        "job_id": job.id,
        "session_id": job.session_id,
        "status": "triggered",
    }))
}

#[tauri::command]
pub async fn job_list_runs(
    id: String,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    let store = tiangong_core::scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
    let runs = store
        .list_job_runs(&id, limit.unwrap_or(20))
        .map_err(|e| e.to_string())?;
    Ok(runs
        .into_iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .collect())
}

// ── Webhook 管理 ─────────────────────────────────────────────

#[tauri::command]
pub async fn webhook_list() -> Result<Vec<serde_json::Value>, String> {
    let store = tiangong_core::scheduler::webhook::store::WebhookStore::open()
        .map_err(|e| e.to_string())?;
    let webhooks = store.list().map_err(|e| e.to_string())?;
    Ok(webhooks
        .into_iter()
        .map(|w| serde_json::to_value(w).unwrap())
        .collect())
}

#[tauri::command]
pub async fn webhook_create(
    name: String,
    description: String,
    session_id: Option<String>,
    payload: String,
    secret: Option<String>,
    enabled: Option<bool>,
) -> Result<serde_json::Value, String> {
    let store = tiangong_core::scheduler::webhook::store::WebhookStore::open()
        .map_err(|e| e.to_string())?;
    let now = chrono::Local::now().naive_local().to_string();
    let webhook = tiangong_core::scheduler::webhook::model::Webhook {
        id: scru128::new().to_string(),
        name,
        description,
        session_id,
        payload,
        secret,
        enabled: enabled.unwrap_or(true),
        created_at: now.clone(),
        updated_at: now,
    };
    store.insert(&webhook).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(webhook).unwrap())
}

#[tauri::command]
pub async fn webhook_update(
    id: String,
    name: Option<String>,
    description: Option<String>,
    session_id: Option<String>,
    payload: Option<String>,
    secret: Option<String>,
    enabled: Option<bool>,
) -> Result<serde_json::Value, String> {
    let store = tiangong_core::scheduler::webhook::store::WebhookStore::open()
        .map_err(|e| e.to_string())?;
    let req = tiangong_core::scheduler::webhook::model::UpdateWebhookRequest {
        name,
        description,
        session_id,
        payload,
        secret,
        enabled,
    };
    let updated = store.update(&id, &req).map_err(|e| e.to_string())?;
    if !updated {
        return Err(format!("Webhook '{id}' 不存在"));
    }
    let webhook = store.get(&id).map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(webhook).unwrap())
}

#[tauri::command]
pub async fn webhook_delete(id: String) -> Result<(), String> {
    let store = tiangong_core::scheduler::webhook::store::WebhookStore::open()
        .map_err(|e| e.to_string())?;
    let deleted = store.delete(&id).map_err(|e| e.to_string())?;
    if !deleted {
        return Err(format!("Webhook '{id}' 不存在"));
    }
    Ok(())
}

#[tauri::command]
pub async fn webhook_trigger(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<serde_json::Value, String> {
    let store = tiangong_core::scheduler::webhook::store::WebhookStore::open()
        .map_err(|e| e.to_string())?;
    let webhook = store
        .get(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Webhook '{id}' 不存在"))?;

    let ctx = state.create_scheduler_context();
    let webhook_clone = webhook.clone();
    tokio::spawn(async move {
        tiangong_scheduler::executor::execute_webhook(ctx, webhook_clone).await;
    });

    Ok(serde_json::json!({
        "webhook_id": webhook.id,
        "session_id": webhook.session_id,
        "status": "triggered",
    }))
}

#[tauri::command]
pub async fn webhook_list_runs(
    id: String,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    let store = tiangong_core::scheduler::webhook::store::WebhookStore::open()
        .map_err(|e| e.to_string())?;
    let runs = store
        .list_runs(&id, limit.unwrap_or(20))
        .map_err(|e| e.to_string())?;
    Ok(runs
        .into_iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::done_event_keeps_turn_running;

    #[test]
    fn empty_done_keeps_pending_turn_running() {
        let event = tiangong_types::StreamEvent::Done { usage: None };
        assert!(done_event_keeps_turn_running(&event, true));
        assert!(!done_event_keeps_turn_running(&event, false));
    }

    #[test]
    fn final_done_with_usage_finishes_pending_turn() {
        let event = tiangong_types::StreamEvent::Done {
            usage: Some(tiangong_types::TokenUsage::default()),
        };
        assert!(!done_event_keeps_turn_running(&event, true));
    }

    #[test]
    fn error_event_never_keeps_turn_running() {
        let event = tiangong_types::StreamEvent::Error {
            message: "failed".to_string(),
        };
        assert!(!done_event_keeps_turn_running(&event, true));
    }
}
