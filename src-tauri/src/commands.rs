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
use tiangong_core::core::SessionMetadataUpdate;
use tracing::{debug, warn};

use crate::workspace_tabs::{
    WorkspaceTabKind as TabKind, WorkspaceTabRef, WorkspaceTabState as TabState,
};

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

fn done_event_keeps_turn_running(
    event: &tiangong_types::StreamEvent,
    has_pending_turn: bool,
) -> bool {
    matches!(event, tiangong_types::StreamEvent::Done { .. }) && has_pending_turn
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
) -> Result<AttachmentDataUrl, String> {
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
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
    .to_string()
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
    let current_assistant_index = session.messages.iter().rposition(|message| {
        message.role == tiangong_core::session::MessageRole::Assistant
            && message
                .tool_calls
                .iter()
                .any(|call| call.id == tool_call_id)
    });
    let current_result = current_assistant_index.and_then(|assistant_index| {
        session.messages[assistant_index + 1..]
            .iter()
            .take_while(|message| message.role == tiangong_core::session::MessageRole::Tool)
            .position(|message| message.tool_call_id.as_deref() == Some(tool_call_id))
            .map(|offset| assistant_index + 1 + offset)
    });
    if let Some(message) = current_result.and_then(|index| session.messages.get_mut(index)) {
        message.content = vec![tiangong_core::session::ContentBlock::text(content)];
        message.tool_name = Some(tool_name.to_string());
        message.tool_result_is_error = is_error;
        session.updated_at = tiangong_core::session::now_text();
        return;
    }
    let message =
        tiangong_core::session::Message::tool_result(tool_call_id, tool_name, content, is_error);
    session.messages.push(message);
    session.updated_at = tiangong_core::session::now_text();
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
) -> Result<tiangong_llm::models_config::ModelCapability, String> {
    tiangong_llm::models_config::ModelCapability::from_key(capability)
        .ok_or_else(|| format!("不支持的能力类型：{capability}"))
}

fn has_capability_in_state(
    core_state: &tiangong_app_state::app_state::TiangongState,
    capability: tiangong_llm::models_config::ModelCapability,
) -> bool {
    core_state.models_config().has_capability(capability)
}

// ============================================================================
// 辅助函数：构建完整的 RunSnapshot
// ============================================================================

pub fn build_full_snapshot_with_status(
    core_state: &tiangong_app_state::app_state::TiangongState,
    is_executing: bool,
) -> RunSnapshotView {
    let sid = core_state.active_session_id();
    build_session_snapshot(core_state, sid, is_executing)
}

fn active_session_is_executing(core_state: &tiangong_app_state::app_state::TiangongState) -> bool {
    let active_id = core_state.active_session_id();
    let snapshot = core_state.run_snapshot();
    core_state.has_pending_turn_for(active_id)
        || (snapshot.last_session_id.as_deref() == Some(active_id)
            && snapshot.status != tiangong_types::RunStatus::Idle)
}

fn build_session_snapshot(
    core_state: &tiangong_app_state::app_state::TiangongState,
    session_id: &str,
    is_session_executing: bool,
) -> RunSnapshotView {
    let core_snapshot = core_state.run_snapshot();
    let input_draft = core_state.session_input_draft(session_id).text;

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
    pub tabs: Vec<TabState>,
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
            if !core_state
                .sessions()
                .iter()
                .any(|session| session.id == session_id)
            {
                return Err(anyhow::anyhow!("会话不存在：{session_id}"));
            }

            let browser =
                tiangong_plugin_browser::session_store::BrowserSessionStore::load(&session_id)?;
            let browser_active = browser.active_tab_id.clone();
            let browser_tabs: Vec<_> = browser
                .tabs
                .iter()
                .map(|tab| TabState {
                    id: tab.id.clone(),
                    kind: TabKind::Browser,
                    title: tab.title.clone(),
                    url: tab.url.clone(),
                    created_at: String::new(),
                })
                .collect();
            let terminal = tiangong_plugin_terminal::session_store::TerminalSessionStore::load_or_migrate_legacy(&session_id)?;
            let terminal_active = terminal.active_tab_id.clone();
            let terminal_tabs: Vec<_> = terminal
                .tabs
                .into_iter()
                .map(|tab| TabState {
                    id: tab.id,
                    kind: TabKind::Terminal,
                    title: tab.title,
                    url: String::new(),
                    created_at: tab.created_at,
                })
                .collect();
            let available = terminal_tabs
                .into_iter()
                .chain(browser_tabs)
                .collect::<Vec<_>>();
            let layout = crate::workspace_tabs::load_layout(&session_id);
            let fallback_active = browser_active
                .map(|id| WorkspaceTabRef {
                    kind: TabKind::Browser,
                    id,
                })
                .into_iter()
                .chain(terminal_active.map(|id| WorkspaceTabRef {
                    kind: TabKind::Terminal,
                    id,
                }))
                .collect::<Vec<_>>();
            let (tabs, active_tab_id) =
                crate::workspace_tabs::reconcile_tabs(available, layout, &fallback_active);
            if let Err(error) = crate::workspace_tabs::save_layout(
                &session_id,
                &tabs,
                active_tab_id.as_deref(),
            ) {
                warn!(%error, session_id, "清理工作区标签页布局失败");
            }

            Ok(SessionTabsView {
                tabs,
                active_tab_id,
            })
        })
        .await
}

/// 写入指定会话的统一工作区 Tab 元数据
#[tauri::command]
pub async fn set_session_tabs(
    session_id: String,
    tabs: Vec<TabState>,
    active_tab_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    state
        .with_state_read(|core_state| {
            if !core_state
                .sessions()
                .iter()
                .any(|session| session.id == session_id)
            {
                return Err(anyhow::anyhow!("会话不存在：{session_id}"));
            }

            crate::workspace_tabs::save_layout(&session_id, &tabs, active_tab_id.as_deref())
        })
        .await
}

/// 创建新会话
#[tauri::command]
pub async fn create_session(state: State<'_, TiangongApp>) -> Result<SessionListItem, String> {
    let result = state
        .with_state(|core_state| {
            core_state.create_session();
            state.mark_active_session_changed();
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

#[derive(Debug, serde::Serialize)]
pub struct DraftSessionCreationView {
    pub session: SessionListItem,
    pub activation_epoch: u64,
    pub previous_active_session_id: String,
}

/// 草稿转正专用：原子写入初始会话配置，但不改变当前活动会话。
#[tauri::command]
pub async fn create_session_for_draft(
    cwd: String,
    trust_mode: String,
    reasoning_effort: String,
    state: State<'_, TiangongApp>,
) -> Result<DraftSessionCreationView, String> {
    let trust_mode = serde_json::from_value(serde_json::Value::String(trust_mode))
        .map_err(|error| format!("无效的信任模式：{error}"))?;
    let valid_efforts = ["none", "low", "medium", "high", "max"];
    if !valid_efforts.contains(&reasoning_effort.as_str()) {
        return Err(format!("无效的思考强度：{reasoning_effort}"));
    }
    state
        .with_state(|core_state| {
            let effective_cwd = if cwd.trim().is_empty() {
                core_state.workspace_dir().to_string()
            } else {
                cwd
            };
            if !std::path::Path::new(&effective_cwd).is_dir() {
                return Err(anyhow::anyhow!("路径不存在或不是目录：{effective_cwd}"));
            }
            let activation_epoch = state.active_session_epoch();
            let previous_active_session_id = core_state.active_session_id().to_string();
            let session = core_state.create_session_without_activation(
                effective_cwd,
                trust_mode,
                reasoning_effort,
            )?;
            Ok(DraftSessionCreationView {
                session: SessionListItem::from_core(&session),
                activation_epoch,
                previous_active_session_id,
            })
        })
        .await
}

/// 只有用户自草稿发送开始后没有切换过会话，才激活转正后的会话。
#[tauri::command]
pub async fn activate_draft_session(
    session_id: String,
    expected_epoch: u64,
    expected_active_session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<bool, String> {
    let activated = state
        .with_state(|core_state| {
            if state.active_session_epoch() != expected_epoch
                || core_state.active_session_id() != expected_active_session_id
            {
                return Ok(false);
            }
            if !core_state
                .sessions()
                .iter()
                .any(|session| session.id == session_id)
            {
                return Err(anyhow::anyhow!("会话不存在：{session_id}"));
            }
            core_state.switch_session(&session_id);
            state.mark_active_session_changed();
            Ok(true)
        })
        .await?;
    if activated {
        state.sync_core_config_from_state().await?;
    }
    Ok(activated)
}

/// 切换到指定会话
#[tauri::command]
pub async fn switch_session(
    session_id: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let (trust_mode, cwd) = state
        .with_state(|core_state| {
            if !core_state
                .sessions()
                .iter()
                .any(|session| session.id == session_id)
            {
                return Err(anyhow::anyhow!("会话不存在：{session_id}"));
            }
            core_state.switch_session(&session_id);
            state.mark_active_session_changed();
            let session = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("会话不存在：{session_id}"))?;
            let cwd = if session.cwd.trim().is_empty() {
                core_state.workspace_dir().to_string()
            } else {
                session.cwd.clone()
            };
            Ok((session.trust_mode, cwd))
        })
        .await?;
    state.sync_core_config_from_state().await?;
    state.set_core_trust_mode(&session_id, trust_mode);

    // 为新会话补充索引（后台执行，不阻塞 UI）
    let sid = session_id.clone();
    let has_session_index = tiangong_plugin_index::session_index_exists(&sid);
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
                && !tiangong_plugin_index::workspace_index_exists(std::path::Path::new(&cwd))
            {
                match tiangong_plugin_index::rebuild_workspace_index_for_gui(std::path::Path::new(
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
                match tiangong_plugin_index::backfill_session_index(&sid, &messages) {
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
                if let Ok(snapshot) =
                    rt.block_on(app_clone.state::<TiangongApp>().with_state_read(|s| {
                        Ok(build_full_snapshot_with_status(
                            s,
                            active_session_is_executing(s),
                        ))
                    }))
                {
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
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
        .await?;
    let _draft_guard = state.draft_update_lock(&deleted_id).lock_owned().await;
    let _send_guard = state.session_send_lock(&deleted_id).lock_owned().await;
    stop_and_join_core(state.inner(), &deleted_id).await;
    // stop_and_join_core 会先从 Core 映射中取走实例，旧流消费者的 EOF 因而不会再
    // 命中 current-instance 清理分支。这里必须主动唤醒内嵌 HTTP 等待者；其 lease
    // 随请求返回释放 remote owner，避免删除后同会话永久保持“远端执行中”。
    state.fail_remote_session_waiters(&deleted_id, "目标会话已删除");
    let mut draft_attachments = state
        .with_state(|core_state| {
            let mut attachments = core_state.session_input_draft(&deleted_id).attachments;
            if let Some(session) = core_state
                .sessions()
                .iter()
                .find(|session| session.id == deleted_id)
            {
                attachments.extend(session_attachment_candidates(session));
            }
            let active_before = core_state.active_session_id().to_string();
            core_state.delete_session_by_id(&deleted_id)?;
            if core_state.active_session_id() != active_before {
                state.mark_active_session_changed();
            }
            Ok(attachments)
        })
        .await?;
    draft_attachments.extend(raw_attachments_for_paths(
        state.release_any_draft_send_claim(&deleted_id),
    ));
    state.remove_session_send_lock(&deleted_id);
    // 删除对话后同步销毁终端和浏览器运行时。
    tiangong_plugin_terminal::destroy_session_pty(&app, &deleted_id);
    app.state::<tiangong_plugin_browser::BrowserPluginState>()
        .registry
        .destroy_session(&deleted_id);
    if let Err(error) = crate::workspace_tabs::remove_layout(&deleted_id) {
        warn!(%error, session_id = %deleted_id, "删除工作区标签页布局失败");
    }
    cleanup_unreferenced_draft_attachments(state.inner(), draft_attachments).await;
    Ok(())
}

/// 删除指定 workspace（cwd）下的所有会话
#[tauri::command]
pub async fn delete_sessions_by_cwd(
    cwd: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let mut deleted_ids = state
        .with_state_read(|core_state| {
            let ids: Vec<String> = core_state
                .sessions()
                .iter()
                .filter(|session| session.cwd == cwd)
                .map(|session| session.id.clone())
                .collect();
            Ok::<_, anyhow::Error>(ids)
        })
        .await?;
    deleted_ids.sort();
    let mut draft_guards = Vec::with_capacity(deleted_ids.len());
    for id in &deleted_ids {
        draft_guards.push(state.draft_update_lock(id).lock_owned().await);
    }
    let mut send_guards = Vec::with_capacity(deleted_ids.len());
    for id in &deleted_ids {
        send_guards.push(state.session_send_lock(id).lock_owned().await);
    }
    for id in &deleted_ids {
        stop_and_join_core(state.inner(), id).await;
        state.fail_remote_session_waiters(id, "目标会话已删除");
    }
    let mut draft_attachments = state
        .with_state(|core_state| {
            let mut candidates = Vec::new();
            let active_before = core_state.active_session_id().to_string();
            for id in &deleted_ids {
                if core_state
                    .sessions()
                    .iter()
                    .any(|session| session.id == *id)
                {
                    candidates.extend(core_state.session_input_draft(id).attachments);
                    if let Some(session) = core_state
                        .sessions()
                        .iter()
                        .find(|session| session.id == *id)
                    {
                        candidates.extend(session_attachment_candidates(session));
                    }
                    core_state.delete_session_by_id(id)?;
                }
            }
            if core_state.active_session_id() != active_before {
                state.mark_active_session_changed();
            }
            Ok(candidates)
        })
        .await?;
    for id in &deleted_ids {
        draft_attachments.extend(raw_attachments_for_paths(
            state.release_any_draft_send_claim(id),
        ));
    }
    // 逐个销毁被删会话的终端和浏览器运行时。
    let browser_state = app.state::<tiangong_plugin_browser::BrowserPluginState>();
    for id in &deleted_ids {
        tiangong_plugin_terminal::destroy_session_pty(&app, id);
        browser_state.registry.destroy_session(id);
        if let Err(error) = crate::workspace_tabs::remove_layout(id) {
            warn!(%error, session_id = %id, "删除工作区标签页布局失败");
        }
        state.remove_session_send_lock(id);
    }
    cleanup_unreferenced_draft_attachments(state.inner(), draft_attachments).await;
    drop(send_guards);
    drop(draft_guards);
    Ok(())
}

pub(crate) async fn stop_and_join_core(state: &TiangongApp, session_id: &str) {
    let _ = state.cancel_core(session_id);
    let Some(core) = state.take_core(session_id) else {
        return;
    };
    let session_id = session_id.to_string();
    match tokio::task::spawn_blocking(move || core.shutdown_join()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(session_id, error = %error, "删除会话前关闭 Core 失败");
        }
        Err(error) => {
            tracing::warn!(session_id, error = %error, "删除会话前等待 Core 关闭任务失败");
        }
    }
}

/// 失败回滚只能关闭本次 ensure 绑定的实例，并且必须等待 worker 最终写盘结束，
/// 才能恢复宿主快照，避免旧 Core 的迟到持久化覆盖回滚结果。
pub(crate) async fn shutdown_join_core_if_current(
    state: &TiangongApp,
    session_id: &str,
    instance_token: &Arc<std::sync::atomic::AtomicBool>,
) {
    let Some(core) = state.take_core_if_current(session_id, instance_token) else {
        return;
    };
    let session_id = session_id.to_string();
    match tokio::task::spawn_blocking(move || core.shutdown_join()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(session_id, error = %error, "失败回滚前关闭 Core 失败");
        }
        Err(error) => {
            tracing::warn!(session_id, error = %error, "失败回滚前等待 Core 关闭任务失败");
        }
    }
}

/// 更新会话标题
#[tauri::command]
pub async fn update_session_title(
    title: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    if title.trim().is_empty() {
        return Err("会话标题不能为空".to_string());
    }

    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
        .await?;
    let session_lock = state.session_send_lock(&session_id);
    let _send_guard = session_lock.lock_owned().await;

    let (previous_title, previous_draft, applied_title) = state
        .with_state(|core_state| {
            if core_state.active_session_id() != session_id {
                return Err(anyhow::anyhow!("活动会话已切换，请重新修改标题"));
            }
            let previous_title = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .map(|session| session.title.clone())
                .ok_or_else(|| anyhow::anyhow!("当前会话不存在，无法重命名"))?;
            let previous_draft = core_state.session_title_draft().to_string();
            core_state.update_session_title_draft(title);
            let (updated_id, applied_title) = core_state.apply_active_session_title_in_memory()?;
            if updated_id != session_id {
                return Err(anyhow::anyhow!("活动会话已切换，请重新修改标题"));
            }
            Ok((previous_title, previous_draft, applied_title))
        })
        .await?;

    let update = SessionMetadataUpdate {
        title: Some(applied_title),
        ..SessionMetadataUpdate::default()
    };
    let receipt = match state.enqueue_session_metadata_update_if_live(&session_id, update) {
        Ok(receipt) => receipt,
        Err(error) => {
            rollback_session_title_mirror(
                state.inner(),
                &session_id,
                previous_title,
                previous_draft,
                false,
            )
            .await;
            return Err(error);
        }
    };

    let persist_without_core = receipt.is_none();
    let persisted = if let Some(receipt) = receipt {
        receipt
            .await_persisted()
            .await
            .map_err(|error| error.to_string())
    } else {
        state
            .with_state(|core_state| core_state.persist_session_and_app(&session_id))
            .await
    };
    if let Err(error) = persisted {
        rollback_session_title_mirror(
            state.inner(),
            &session_id,
            previous_title,
            previous_draft,
            persist_without_core,
        )
        .await;
        return Err(error);
    }
    Ok(())
}

async fn rollback_session_title_mirror(
    state: &TiangongApp,
    session_id: &str,
    previous_title: String,
    previous_draft: String,
    persist_without_core: bool,
) {
    let rollback = state
        .with_state(|core_state| {
            let is_active = core_state.active_session_id() == session_id;
            if let Some(session) = core_state
                .sessions_mut()
                .iter_mut()
                .find(|session| session.id == session_id)
            {
                session.title = previous_title;
            }
            if is_active {
                core_state.update_session_title_draft(previous_draft);
            }
            if persist_without_core {
                core_state.persist_session_and_app(session_id)?;
            }
            Ok(())
        })
        .await;
    if let Err(error) = rollback {
        warn!(%error, %session_id, "回滚会话标题失败");
    }
}

// ============================================================================
// 消息和执行
// ============================================================================

#[derive(Clone, Copy)]
enum UserMessageDeliveryKind {
    NewTurn,
    Append,
}

struct UserMessageDeliveryRequest {
    session_id: String,
    content: String,
    attachments: Vec<tiangong_media_archive::RawAttachment>,
    revision: u64,
    delivery_kind: UserMessageDeliveryKind,
    requires_draft_claim: bool,
}

/// 发送消息并执行
#[tauri::command]
pub async fn send_message(
    session_id: String,
    content: String,
    attachments: Vec<tiangong_media_archive::RawAttachment>,
    revision: u64,
    app: AppHandle,
    _window: Window,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    send_message_inner(
        UserMessageDeliveryRequest {
            session_id,
            content,
            attachments,
            revision,
            delivery_kind: UserMessageDeliveryKind::NewTurn,
            requires_draft_claim: true,
        },
        app,
        state.inner(),
    )
    .await
}

/// 旧前端兼容入口。新代码统一调用带 session_id/revision 的 `send_message`。
#[tauri::command]
pub async fn send_message_with_media(
    content: String,
    media: Vec<tiangong_types::MediaAsset>,
    app: AppHandle,
    _window: Window,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let (session_id, revision) = state
        .with_state_read(|core_state| {
            let session_id = core_state.active_session_id().to_string();
            let revision = core_state.session_input_draft(&session_id).revision;
            Ok((session_id, revision))
        })
        .await?;
    let attachments = media
        .into_iter()
        .map(|asset| tiangong_media_archive::RawAttachment {
            kind: asset.kind,
            source: asset.url,
            mime_type: asset.mime_type,
            original_name: asset.title,
        })
        .collect();
    send_message_inner(
        UserMessageDeliveryRequest {
            session_id,
            content,
            attachments,
            revision,
            delivery_kind: UserMessageDeliveryKind::NewTurn,
            requires_draft_claim: false,
        },
        app,
        state.inner(),
    )
    .await
}

async fn send_message_inner(
    request: UserMessageDeliveryRequest,
    app: AppHandle,
    state: &TiangongApp,
) -> Result<(), String> {
    use std::sync::mpsc;
    use tiangong_types::SessionStreamEvent;

    let UserMessageDeliveryRequest {
        session_id,
        content,
        attachments,
        revision,
        delivery_kind,
        requires_draft_claim,
    } = request;

    if session_id.trim().is_empty() {
        return Err("目标会话 ID 不能为空".to_string());
    }

    if let Some(command) = parse_context_slash_command(&content) {
        let active_id = state
            .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
            .await?;
        if active_id != session_id {
            return Err("目标会话已切换，请在原会话中重试该命令".to_string());
        }
        run_context_slash_command(command, app, state).await?;
        return Ok(());
    }

    let session_lock = state.session_send_lock(&session_id);
    let _send_guard = session_lock.lock().await;

    if state.remote_turn_owner(&session_id).is_some() {
        abort_session_send(state, &session_id, revision, false).await;
        return Err("目标会话正在处理远端请求，请等待本轮完成".to_string());
    }

    if requires_draft_claim && !state.has_draft_send_claim(&session_id, revision) {
        abort_session_send(state, &session_id, revision, false).await;
        return Err("发送草稿尚未冻结，请基于最新输入重试".to_string());
    }

    let has_pending_turn = state
        .with_state_read(|core_state| Ok(core_state.has_pending_turn_for(&session_id)))
        .await?;
    match delivery_kind {
        UserMessageDeliveryKind::NewTurn if has_pending_turn => {
            abort_session_send(state, &session_id, revision, false).await;
            return Err("目标会话已在执行，请使用追加消息".to_string());
        }
        UserMessageDeliveryKind::Append if !has_pending_turn => {
            abort_session_send(state, &session_id, revision, false).await;
            return Err("目标会话当前没有可追加的执行任务".to_string());
        }
        _ => {}
    }
    if state.draft_revision_was_delivered(&session_id, revision) {
        abort_session_send(state, &session_id, revision, false).await;
        return Err("该版本草稿已成功发送，已拒绝重复投递".to_string());
    }

    if let Err(error) = state.sync_core_config_from_state().await {
        abort_session_send(state, &session_id, revision, false).await;
        return Err(error);
    }
    if let Err(error) = state
        .with_state(|core_state| core_state.begin_session_send(&session_id, revision))
        .await
    {
        abort_session_send(state, &session_id, revision, false).await;
        return Err(error);
    }

    let capabilities = match attachment_capability_snapshot(state).await {
        Ok(value) => value,
        Err(error) => {
            abort_session_send(state, &session_id, revision, true).await;
            return Err(error);
        }
    };
    let user_message_id = scru128::new().to_string();
    let message_id_for_prepare = user_message_id.clone();
    let content_for_prepare = content.clone();
    let prepared_batch = tokio::task::spawn_blocking(move || {
        let store = tiangong_media_archive::AttachmentStore::default();
        let mut transaction = store.store_batch(attachments)?;
        let prepared = transaction.prepare_message(
            &message_id_for_prepare,
            content_for_prepare,
            capabilities,
        )?;
        Ok::<_, String>((transaction, prepared))
    })
    .await
    .map_err(|error| format!("附件准备任务失败：{error}"));
    let (transaction, prepared) = match prepared_batch {
        Ok(Ok(value)) => value,
        Ok(Err(error)) | Err(error) => {
            abort_session_send(state, &session_id, revision, true).await;
            return Err(format!("附件准备失败：{error}"));
        }
    };

    let prepared_for_state = tiangong_types::stable_content_blocks(&prepared);
    let state_prepare = state
        .with_state(|core_state| {
            let index = core_state
                .sessions()
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("目标会话不存在：{session_id}"))?;
            let original_session = core_state.sessions()[index].clone();
            core_state.sessions_mut()[index]
                .append_prepared_user_message_with_id(user_message_id.clone(), prepared_for_state);
            core_state.sessions_mut()[index].updated_at = tiangong_core::session::now_text();
            core_state.mark_pending_message_for(&session_id, &user_message_id);
            if let Err(error) = core_state.persist_session_and_app(&session_id) {
                core_state.sessions_mut()[index] = original_session;
                core_state.remove_pending_message_for(&session_id, &user_message_id);
                let rollback_error = core_state.persist_session_and_app(&session_id).err();
                return Err(match rollback_error {
                    Some(rollback_error) => anyhow::anyhow!(
                        "消息状态持久化失败：{error}；恢复原状态也失败：{rollback_error}"
                    ),
                    None => anyhow::anyhow!("消息状态持久化失败：{error}"),
                });
            }
            let mut runtime_session = core_state.sessions()[index].clone();
            if runtime_session.cwd.trim().is_empty() {
                runtime_session.cwd = core_state.workspace_dir().to_string();
            }
            Ok(runtime_session)
        })
        .await;
    let session_snapshot = match state_prepare {
        Ok(value) => value,
        Err(error) => {
            abort_session_send(state, &session_id, revision, true).await;
            return Err(error);
        }
    };

    // App 稳定消息已成功落盘，附件所有权从临时事务转移给该消息。
    // 后续 future 即使被取消也不会因 Drop 删掉已被稳定状态引用的文件。
    let created_paths = transaction
        .newly_created_paths()
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    transaction.commit();

    // 获取或创建 TiangongCore
    let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();
    let ensured = state
        .ensure_core(&session_id, session_snapshot, stream_tx)
        .await;
    let sid = ensured.session_id.clone();
    let receipt_result = state.enqueue_prepared_with_receipt_if_current(
        &sid,
        &ensured.instance_token,
        user_message_id.clone(),
        prepared.clone(),
    );
    let receipt = match receipt_result {
        Ok(receipt) => receipt,
        Err(error) => {
            shutdown_join_core_if_current(state, &sid, &ensured.instance_token).await;
            let _ = restore_failed_user_message_state(state, &session_id, &user_message_id).await;
            cleanup_unreferenced_draft_attachments(
                state,
                raw_attachments_for_paths(created_paths.clone()),
            )
            .await;
            abort_session_send(state, &session_id, revision, true).await;
            return Err(format!("消息投递失败：{error}"));
        }
    };

    if let Err(error) = receipt.await_persisted().await {
        shutdown_join_core_if_current(state, &sid, &ensured.instance_token).await;
        let _ = restore_failed_user_message_state(state, &session_id, &user_message_id).await;
        cleanup_unreferenced_draft_attachments(state, raw_attachments_for_paths(created_paths))
            .await;
        abort_session_send(state, &session_id, revision, true).await;
        return Err(format!("消息投递失败：{error}"));
    }

    // Core 与 App 的稳定消息均已落盘，附件从此由消息引用持有，不能再自动回滚。
    state.mark_draft_revision_delivered(&session_id, revision);
    let finish_result = state
        .with_state(|core_state| {
            if core_state.has_pending_turn_for(&session_id)
                && core_state.active_session_id() == session_id
            {
                core_state.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
                core_state.store.runtime.run.summary = "正在处理".to_string();
                core_state.store.runtime.run.last_session_id = Some(session_id.clone());
                if let Some(session) = core_state
                    .sessions()
                    .iter()
                    .find(|session| session.id == session_id)
                {
                    let usage = session.total_usage();
                    core_state.store.runtime.run.last_usage =
                        (usage.total_tokens > 0).then_some(usage);
                }
                core_state.store.runtime.run.updated_at = tiangong_core::session::now_text();
            }
            core_state.finish_session_send(&session_id, revision, true)
        })
        .await;
    if let Err(error) = finish_result {
        // Core 已确认稳定消息，此时不能把已发送的消息误报为可重试失败。
        // finish_session_send 在写盘前已更新内存，记录磁盘错误供诊断。
        tracing::error!(session_id, revision, error = %error, "消息已发送，但草稿终态持久化失败");
    }
    release_draft_send_claim_and_cleanup(state, &session_id, revision).await;

    if ensured.is_new {
        start_stream_consumer(app, sid, stream_rx, ensured.instance_token);
    }

    Ok(())
}

async fn abort_session_send(state: &TiangongApp, session_id: &str, revision: u64, began: bool) {
    if began {
        let _ = state
            .with_state(|core_state| core_state.finish_session_send(session_id, revision, false))
            .await;
    }
    release_draft_send_claim_and_cleanup(state, session_id, revision).await;
}

async fn release_draft_send_claim_and_cleanup(
    state: &TiangongApp,
    session_id: &str,
    revision: u64,
) {
    let paths = state.release_draft_send_claim(session_id, revision);
    if !paths.is_empty() {
        cleanup_unreferenced_draft_attachments(state, raw_attachments_for_paths(paths)).await;
    }
}

pub(crate) async fn restore_failed_user_message_state(
    state: &TiangongApp,
    session_id: &str,
    message_id: &str,
) -> Result<(), String> {
    state
        .with_state(|core_state| {
            let session = core_state
                .sessions_mut()
                .iter_mut()
                .find(|session| session.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("目标会话已不存在：{session_id}"))?;
            if let Some(index) = session
                .messages
                .iter()
                .position(|message| message.id == message_id)
            {
                session.messages.remove(index);
                session.summary_up_to = session.summary_up_to.min(session.messages.len());
                session.updated_at = tiangong_core::session::now_text();
            }
            core_state.remove_pending_message_for(session_id, message_id);
            core_state.persist_session_and_app(session_id)
        })
        .await
}

pub(crate) async fn attachment_capability_snapshot(
    state: &TiangongApp,
) -> Result<tiangong_media_archive::AttachmentCapabilitySnapshot, String> {
    state
        .with_state_read(|core_state| {
            use tiangong_llm::ModelCapability;
            let models = core_state.models_config();
            let chat_multimodal = models.chat_is_multimodal();
            Ok(tiangong_media_archive::AttachmentCapabilitySnapshot {
                chat_multimodal,
                analyze_attachment: !chat_multimodal
                    && models
                        .resolve_for_capability(ModelCapability::Multimodal)
                        .is_some(),
                audio_processor: models
                    .resolve_for_capability(ModelCapability::Stt)
                    .is_some(),
                // 当前没有“视频内容分析”插件；视频生成能力不能冒充输入处理能力。
                video_processor: false,
            })
        })
        .await
}

/// 消费 SessionStreamEvent：emit 给前端 + 更新 RunStatus + Done 时同步 session
pub(crate) fn start_stream_consumer(
    app: AppHandle,
    session_id: String,
    stream_rx: std::sync::mpsc::Receiver<tiangong_types::SessionStreamEvent>,
    instance_token: Arc<std::sync::atomic::AtomicBool>,
) {
    use tiangong_types::StreamEvent;

    let rt = tokio::runtime::Handle::current();
    thread::spawn(move || {
        let mut assistant_msg_id: Option<String> = None;
        let mut last_tool_args_summary = String::new();
        let mut remote_turn_correlation = crate::embedded_server::RemoteTurnCorrelation::default();
        for session_event in stream_rx.iter() {
            let app_state = app.state::<TiangongApp>();
            let event_lock = app_state.session_send_lock(&session_id);
            let _event_guard = rt.block_on(event_lock.lock_owned());
            if !app_state.is_current_core_instance(&session_id, &instance_token) {
                // 已退役 Core 的缓冲事件不得修改同会话新实例的 pending、消息或 UI。
                continue;
            }
            let event = &session_event.event;

            // 取消时跳过文本增量事件，只处理终止事件
            if instance_token.load(Ordering::Acquire)
                && matches!(
                    event,
                    StreamEvent::Delta { .. }
                        | StreamEvent::ReactText { .. }
                        | StreamEvent::SummaryText { .. }
                        | StreamEvent::Reasoning { .. }
                )
            {
                continue;
            }

            let terminal_event = matches!(
                &session_event.event,
                StreamEvent::Done { .. } | StreamEvent::Error { .. }
            );
            // 普通流事件立即转发；终态必须等 Core 权威会话重载完成后再对外发布。
            if !terminal_event {
                let _ = app.emit("stream_event", &session_event);
            }

            let sid = session_event.session_id;
            let event = session_event.event;
            let is_done = matches!(event, StreamEvent::Done { .. });
            let is_error = matches!(event, StreamEvent::Error { .. });
            let completed_remote_message_id = remote_turn_correlation.observe(&event);

            // 更新 session + RunStatus/usage
            let _ = rt.block_on(app.state::<TiangongApp>().with_state(|core_state| {
                let accepted_message_id = match &event {
                    StreamEvent::UserMessage { message_id, .. } => Some(message_id.clone()),
                    _ => None,
                };
                let final_user_snapshot = matches!(
                    &event,
                    StreamEvent::SessionMessageUpsert { message, .. }
                        if message.role == tiangong_core::session::MessageRole::User
                            && message.turn_status.is_some()
                );
                if let Some(session) = core_state.sessions_mut().iter_mut().find(|s| s.id == sid) {
                    match &event {
                        StreamEvent::UserMessage {
                            message_id,
                            content,
                            content_blocks,
                            media,
                            model_excluded,
                            pending_plugin_deliveries,
                        } => {
                            let prepared = if content_blocks.is_empty() {
                                let mut blocks = vec![tiangong_types::message::ContentBlock::text(
                                    content.clone(),
                                )];
                                blocks.extend(media.iter().map(|asset| asset.to_content_block()));
                                tiangong_types::stable_content_blocks(&blocks)
                            } else {
                                tiangong_types::stable_content_blocks(content_blocks)
                            };
                            let existing = session
                                .messages
                                .iter()
                                .position(|message| {
                                    message.id == *message_id
                                        && message.role == tiangong_core::session::MessageRole::User
                                })
                                .map(|index| session.messages.remove(index));
                            if let Some(mut message) = existing {
                                // 兼容旧 Core 的状态更新事件：没有稳定块和 media 时
                                // 保留已同步内容，仅更新可见性并移动到轮次末尾。
                                if !content_blocks.is_empty()
                                    || !media.is_empty()
                                    || message.content.is_empty()
                                    || message.text_content() != *content
                                {
                                    message.content = prepared;
                                }
                                message.model_excluded = *model_excluded;
                                session.messages.push(message);
                            } else {
                                session.append_prepared_user_message_with_id(
                                    message_id.clone(),
                                    prepared,
                                );
                                session.set_message_model_excluded(message_id, *model_excluded);
                            }
                            session.pending_plugin_deliveries = pending_plugin_deliveries.clone();
                        }
                        StreamEvent::SessionMessageUpsert {
                            message,
                            pending_plugin_deliveries,
                            completed_plugin_delivery_ids,
                            deferred_tool_injections,
                        } => {
                            if let Some(existing) = session
                                .messages
                                .iter_mut()
                                .find(|existing| existing.id == message.id)
                            {
                                *existing = message.clone();
                            } else {
                                session.messages.push(message.clone());
                            }
                            if let Some(deliveries) = pending_plugin_deliveries {
                                session.pending_plugin_deliveries = deliveries.clone();
                            }
                            if let Some(delivery_ids) = completed_plugin_delivery_ids {
                                session.completed_plugin_delivery_ids = delivery_ids.clone();
                            }
                            if let Some(injections) = deferred_tool_injections {
                                session.deferred_tool_injections = injections.clone();
                            }
                        }
                        StreamEvent::PendingPluginDeliveriesChanged {
                            deliveries,
                            completed_delivery_ids,
                        } => {
                            session.pending_plugin_deliveries = deliveries.clone();
                            session.completed_plugin_delivery_ids = completed_delivery_ids.clone();
                        }
                        StreamEvent::DeferredToolInjectionsChanged { injections } => {
                            session.deferred_tool_injections = injections.clone();
                        }
                        StreamEvent::Delta {
                            message_id,
                            content,
                        }
                        | StreamEvent::ReactText {
                            message_id,
                            content,
                        }
                        | StreamEvent::SummaryText {
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
                            names: _,
                            calls,
                            usage: _,
                        } => {
                            finalize_assistant_tool_calls(
                                session,
                                &mut assistant_msg_id,
                                message_id,
                                calls,
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
                            duration_ms: _,
                        } => {
                            let persisted_output = full_output.as_deref().unwrap_or(output);

                            // plugin_injection 的完整 Assistant+Tool 对由 SessionMessageUpsert
                            // 先行同步；这里按 tool_call_id 原位更新，普通结果则追加。
                            append_tool_result_message(
                                session,
                                tool_call_id.as_deref(),
                                name,
                                persisted_output.to_string(),
                                !*ok,
                            );
                            last_tool_args_summary.clear();
                        }
                        StreamEvent::ApprovalNeeded { .. } => {
                            // 审批请求不写入 session（前端通过 RunStatus 展示审批 UI）
                        }
                        StreamEvent::Error { ref message } => {
                            let _ = message;
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
                        StreamEvent::MemoryRecallStart { .. } => {
                            // 仅更新状态栏（见下方状态映射），不写入对话消息列表
                        }
                        StreamEvent::MemoryRecallDone { .. } => {
                            // 仅更新状态栏（见下方状态映射），不写入对话消息列表
                        }
                        StreamEvent::AgentCreated {
                            agent_id: _,
                            role: _,
                            label: _,
                        } => {}
                        StreamEvent::AgentStatusChanged {
                            ref agent_id,
                            label: _,
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
                        }
                        StreamEvent::AgentNotification {
                            ref agent_label,
                            ref content,
                            ..
                        } => {
                            // 通知只更新运行状态；稳定对话消息由 Core 的精确快照负责。
                            let _ = (agent_label, content);
                        }
                        StreamEvent::AgentMessage {
                            ref from_agent_id,
                            ref from_agent_label,
                            ref to_agent_id,
                            ref content,
                            ..
                        } => {
                            if from_agent_id == "user" {
                                // 用户 @Agent 的原始输入已经作为用户消息存在，避免重复写入。
                            } else if to_agent_id == "main" {
                                // 最终回复由 SessionMessageUpsert 携带完整消息写入，避免宿主
                                // 生成不同 ID 或丢失模型可见性标记。
                                let _ = (from_agent_label, content);
                            }
                        }
                        StreamEvent::AgentOutput {
                            ref agent_id,
                            ref agent_role,
                            ref agent_label,
                            ref messages,
                        } => {
                            // Sub Agent 的内部对话（工具调用、过程文本、中间推理）以 worker_id
                            // 标记写入 session.messages，主对话视图按 worker_id 过滤不显示，
                            // 但 GUI 顶部 Agent Tab 切换时可查看子 Agent 的完整执行过程。
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
                        }
                        _ => {}
                    }
                }
                if let Some(message_id) = accepted_message_id {
                    core_state.accept_pending_message_for(&sid, &message_id);
                }
                if final_user_snapshot {
                    core_state.complete_accepted_turn_for(&sid);
                }
                // RunStatus/usage 更新
                match &event {
                    StreamEvent::UserMessage { .. } => {
                        core_state.store.runtime.run.status =
                            tiangong_core::runtime::RunStatus::Executing;
                        core_state.store.runtime.run.summary = "正在处理".to_string();
                        core_state.store.runtime.run.last_session_id = Some(sid.clone());
                        core_state.store.runtime.run.updated_at =
                            tiangong_core::session::now_text();
                    }
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
                        if core_state.has_pending_turn_for(&sid) {
                            core_state.store.runtime.run.status =
                                tiangong_core::runtime::RunStatus::Executing;
                            core_state.store.runtime.run.summary = "正在处理下一条消息".to_string();
                            core_state.store.runtime.run.last_session_id = Some(sid.clone());
                            core_state.store.runtime.run.updated_at =
                                tiangong_core::session::now_text();
                        } else {
                            core_state.report_run_idle(format!("执行失败：{message}"));
                            core_state.clear_pending_turn_for(&sid);
                        }
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
                    StreamEvent::Delta { .. }
                    | StreamEvent::ReactText { .. }
                    | StreamEvent::SummaryText { .. } => {
                        core_state.store.runtime.run.summary = "正在回复...".to_string();
                    }
                    StreamEvent::PhaseChanged { .. } => {}
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

            // 终态会在权威重载后发布最终快照；避免先暴露临时 reducer 镜像。
            if !is_done && !is_error {
                if let Ok(snapshot) =
                    rt.block_on(app.state::<TiangongApp>().with_state_read(|core_state| {
                        Ok(build_full_snapshot_with_status(
                            core_state,
                            active_session_is_executing(core_state),
                        ))
                    }))
                {
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
                // Done：先持久化，再异步生成标题（不阻塞消费线程）
                let final_sid = sid.clone();

                // 提取标题生成所需数据（在锁内完成，避免长时间持锁）
                let title_task = rt.block_on(app.state::<TiangongApp>().with_state(|core_state| {
                    // Core 是执行中会话的唯一真相。终态已经在 Core 完成清理和落盘后
                    // 发出，此处整会话重载，覆盖宿主为流式展示构造的临时消息。
                    if let Err(error) = core_state.reload_session_from_disk(&final_sid) {
                        tracing::warn!(%error, session_id = %final_sid, "终态重载 Core 会话失败");
                    }
                    let _ = core_state.persist_app_only();
                    let still_running = done_event_keeps_turn_running(
                        &event,
                        core_state.has_pending_turn_for(&final_sid),
                    );
                    let snapshot = build_full_snapshot_with_status(
                        core_state,
                        active_session_is_executing(core_state),
                    );
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
                                    .resolve_slot(tiangong_llm::models_config::RoutingSlot::Lite)
                                    .or_else(|| {
                                        core_state.store.provider.models_config.resolve_slot(
                                            tiangong_llm::models_config::RoutingSlot::Chat,
                                        )
                                    })
                                    .map(tiangong_llm::ModelEndpoint::from_resolved)
                                    .unwrap_or_else(|| {
                                        core_state.store.provider.model_endpoint.clone()
                                    });
                                return Ok(Some((input, provider_config)));
                            }
                        }
                    }
                    Ok(None)
                }));

                if let Some(message_id) = completed_remote_message_id {
                    rt.block_on(crate::embedded_server::complete_remote_turn_from_stream(
                        app.state::<TiangongApp>().inner(),
                        &final_sid,
                        &message_id,
                    ));
                }

                let _ = app.emit(
                    "stream_event",
                    &tiangong_types::SessionStreamEvent {
                        session_id: final_sid.clone(),
                        event: event.clone(),
                    },
                );

                // 异步生成标题（不阻塞消费线程）
                if let Ok(Some((input, provider_config))) = title_task {
                    let app_for_title = app.clone();
                    let sid_for_title = final_sid.clone();
                    let title_instance_token = instance_token.clone();
                    let rt2 = rt.clone();
                    thread::spawn(move || {
                        let client =
                            tiangong_core::model::SingleProviderClient::new(provider_config);
                        if let Ok(t) = client.complete_lite(&input) {
                            let clean = t.trim().trim_matches('"').to_string();
                            if !clean.is_empty() {
                                let app_state = app_for_title.state::<TiangongApp>();
                                let title_lock = app_state.session_send_lock(&sid_for_title);
                                let _title_guard = rt2.block_on(title_lock.lock_owned());
                                let title_is_still_default = rt2
                                    .block_on(app_state.with_state_read(|core_state| {
                                        Ok(core_state
                                            .sessions()
                                            .iter()
                                            .find(|session| session.id == sid_for_title)
                                            .is_some_and(|session| {
                                                session.title == "新对话"
                                                    || session.title.starts_with("会话 ")
                                            }))
                                    }))
                                    .unwrap_or(false);
                                if !title_is_still_default {
                                    return;
                                }
                                let receipt = match app_state
                                    .enqueue_session_metadata_update_if_current(
                                        &sid_for_title,
                                        &title_instance_token,
                                        SessionMetadataUpdate {
                                            title: Some(clean.clone()),
                                            ..SessionMetadataUpdate::default()
                                        },
                                    ) {
                                    Ok(Some(receipt)) => receipt,
                                    Ok(None) => return,
                                    Err(error) => {
                                        warn!(
                                            %error,
                                            session_id = %sid_for_title,
                                            "自动标题入队失败"
                                        );
                                        return;
                                    }
                                };
                                if let Err(error) = rt2.block_on(receipt.await_persisted()) {
                                    warn!(
                                        %error,
                                        session_id = %sid_for_title,
                                        "自动标题持久化失败"
                                    );
                                    return;
                                }
                                if !app_state
                                    .is_current_core_instance(&sid_for_title, &title_instance_token)
                                {
                                    return;
                                }
                                let _ = rt2.block_on(app_state.with_state(|core_state| {
                                    let is_active = core_state.active_session_id() == sid_for_title;
                                    if let Some(s) = core_state
                                        .sessions_mut()
                                        .iter_mut()
                                        .find(|s| s.id == sid_for_title)
                                    {
                                        s.title = clean.clone();
                                        s.updated_at = tiangong_core::session::now_text();
                                    }
                                    if is_active {
                                        core_state.update_session_title_draft(clean.clone());
                                    }
                                    let snapshot = build_full_snapshot_with_status(
                                        core_state,
                                        active_session_is_executing(core_state),
                                    );
                                    let _ = app_for_title.emit("run_snapshot", &snapshot);
                                    let _ = app_for_title.emit("sessions_updated", &());
                                    Ok(())
                                }));
                            }
                        }
                    });
                }
                // 不 break — 消费线程继续运行，等待下一轮消息的 StreamEvent
            }
        }

        let app_state = app.state::<TiangongApp>();
        let eof_lock = app_state.session_send_lock(&session_id);
        let _eof_guard = rt.block_on(eof_lock.lock_owned());
        if app_state.remove_stopped_core_if_current(&session_id, &instance_token) {
            let _ = rt.block_on(app_state.with_state(|core_state| {
                if let Err(error) = core_state.reload_session_from_disk(&session_id) {
                    tracing::warn!(%error, %session_id, "Core 事件流关闭后重载会话失败");
                }
                core_state.clear_pending_turn_for(&session_id);
                core_state.report_run_idle("执行已中断：Core 事件流已关闭".to_string());
                let _ = core_state.persist_app_only();
                let snapshot = build_full_snapshot_with_status(
                    core_state,
                    active_session_is_executing(core_state),
                );
                let _ = app.emit("run_snapshot", &snapshot);
                let _ = app.emit("sessions_updated", &());
                Ok(())
            }));
            app_state.fail_remote_session_waiters(&session_id, "执行已中断：Core 事件流已关闭");
            let _ = app.emit(
                "stream_event",
                &tiangong_types::SessionStreamEvent {
                    session_id,
                    event: tiangong_types::StreamEvent::Error {
                        message: "Core 事件流已关闭".to_string(),
                    },
                },
            );
        }
    });
}

/// 截断该消息之后的所有内容，更新消息内容，然后创建新的 core 重新执行 turn。
// Tauri 命令参数保持与现有前端调用协议一一对应，避免把传输层字段额外嵌套。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn edit_and_resend(
    session_id: String,
    message_id: String,
    new_content: String,
    attachments: Vec<tiangong_media_archive::RawAttachment>,
    revision: u64,
    base_content: Vec<tiangong_types::ContentBlock>,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    use std::sync::mpsc;

    if session_id.trim().is_empty() || message_id.trim().is_empty() {
        return Err("会话 ID 和消息 ID 不能为空".to_string());
    }
    if new_content.trim().is_empty() && attachments.is_empty() {
        return Err("编辑后的消息不能为空".to_string());
    }
    if base_content.is_empty() {
        return Err("编辑基线版本不能为空".to_string());
    }
    tracing::debug!(session_id, message_id, revision, "开始编辑重发校验");
    let session_lock = state.session_send_lock(&session_id);
    let _send_guard = session_lock.lock().await;
    if state.remote_turn_owner(&session_id).is_some() {
        return Err("目标会话正在处理远端请求，暂时不能编辑重发".to_string());
    }
    let has_pending_turn = state
        .with_state_read(|core_state| Ok(core_state.has_pending_turn_for(&session_id)))
        .await?;
    if has_pending_turn {
        return Err("目标会话正在执行，暂时不能编辑重发".to_string());
    }
    state.sync_core_config_from_state().await?;

    // 第一遍只读校验发生在任何附件 IO 之前。
    state
        .with_state_read(|core_state| {
            let session = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("会话不存在：{session_id}"))?;
            if core_state.has_pending_turn_for(&session_id) {
                return Err(anyhow::anyhow!("目标会话正在执行，暂时不能编辑重发"));
            }
            validate_editable_message(session, &message_id, &base_content)?;
            Ok(())
        })
        .await?;

    let capabilities = attachment_capability_snapshot(state.inner()).await?;
    let content_for_prepare = new_content.clone();
    let message_id_for_prepare = message_id.clone();
    let (transaction, prepared) = tokio::task::spawn_blocking(move || {
        let store = tiangong_media_archive::AttachmentStore::default();
        let mut transaction = store.store_batch(attachments)?;
        let prepared = transaction.prepare_message(
            &message_id_for_prepare,
            content_for_prepare,
            capabilities,
        )?;
        Ok::<_, String>((transaction, prepared))
    })
    .await
    .map_err(|error| format!("附件准备任务失败：{error}"))?
    .map_err(|error| format!("附件准备失败：{error}"))?;

    // 附件准备可能很慢；在同一把 App 状态锁内做完整二次校验、修改和持久化。
    // 只有本步骤成功后才允许终止旧任务。
    let prepared_for_state = tiangong_types::stable_content_blocks(&prepared);
    let (original_session, originally_pending, session_snapshot) = state
        .with_state(|core_state| {
            let index = core_state
                .sessions()
                .iter()
                .position(|session| session.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("会话不存在：{session_id}"))?;
            if core_state.has_pending_turn_for(&session_id) {
                return Err(anyhow::anyhow!("目标会话正在执行，暂时不能编辑重发"));
            }
            validate_editable_message(&core_state.sessions()[index], &message_id, &base_content)?;
            let original_session = core_state.sessions()[index].clone();
            let originally_pending = core_state.has_pending_turn_for(&session_id);
            let session = &mut core_state.sessions_mut()[index];
            if !session.update_prepared_user_message(&message_id, prepared_for_state) {
                return Err(anyhow::anyhow!("消息不存在：{message_id}"));
            }
            session.truncate_after_message(&message_id);
            session.updated_at = tiangong_core::session::now_text();

            let mut runtime_session = session.clone();
            if runtime_session.cwd.trim().is_empty() {
                runtime_session.cwd = core_state.workspace_dir().to_string();
            }

            core_state.mark_pending_message_for(&session_id, &message_id);
            if let Err(error) = core_state.persist_session_and_app(&session_id) {
                core_state.sessions_mut()[index] = original_session;
                core_state.remove_pending_message_for(&session_id, &message_id);
                let rollback_error = core_state.persist_session_and_app(&session_id).err();
                return Err(match rollback_error {
                    Some(rollback_error) => anyhow::anyhow!(
                        "编辑状态持久化失败：{error}；恢复原状态也失败：{rollback_error}"
                    ),
                    None => anyhow::anyhow!("编辑状态持久化失败：{error}"),
                });
            }

            Ok((original_session, originally_pending, runtime_session))
        })
        .await?;

    let replaced_attachment_candidates = original_session
        .messages
        .iter()
        .flat_map(|message| {
            message
                .extract_media_assets()
                .into_iter()
                .map(|asset| asset.url)
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    // 编辑后的稳定 App 消息已落盘，立即转移附件所有权，避免异步
    // future 被取消时 Drop 误删已被稳定消息引用的新文件。
    let created_paths = transaction
        .newly_created_paths()
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    transaction.commit();

    // 最终校验及稳定消息落盘均成功后，才终止旧 Core。
    if let Some(core) = state.take_core(&session_id) {
        let _ = core.deliver(AgentInputKind::cancel());
        match tokio::task::spawn_blocking(move || core.shutdown_join()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, %session_id, "编辑重发前关闭旧 Core 失败"),
            Err(error) => tracing::warn!(%error, %session_id, "编辑重发前等待旧 Core 失败"),
        }
        // 旧 Core 的最终收尾可能最后一次写回编辑前快照；join 后重新落盘当前编辑状态，
        // 此后已无旧写入者可覆盖。
        state
            .with_state(|core_state| core_state.persist_session_and_app(&session_id))
            .await?;
    }

    let (stream_tx, stream_rx) = mpsc::channel::<tiangong_types::SessionStreamEvent>();
    let ensured = state
        .ensure_core(&session_id, session_snapshot, stream_tx)
        .await;
    let sid = ensured.session_id.clone();
    let receipt_result = state.enqueue_prepared_with_receipt_if_current(
        &sid,
        &ensured.instance_token,
        message_id.clone(),
        prepared.clone(),
    );
    let receipt = match receipt_result {
        Ok(receipt) => receipt,
        Err(error) => {
            shutdown_join_core_if_current(state.inner(), &sid, &ensured.instance_token).await;
            restore_edited_session(
                state.inner(),
                &session_id,
                original_session,
                originally_pending,
            )
            .await;
            cleanup_unreferenced_draft_attachments(
                state.inner(),
                raw_attachments_for_paths(created_paths.clone()),
            )
            .await;
            return Err(format!("编辑消息投递失败：{error}"));
        }
    };
    if let Err(error) = receipt.await_persisted().await {
        shutdown_join_core_if_current(state.inner(), &sid, &ensured.instance_token).await;
        restore_edited_session(
            state.inner(),
            &session_id,
            original_session,
            originally_pending,
        )
        .await;
        cleanup_unreferenced_draft_attachments(
            state.inner(),
            raw_attachments_for_paths(created_paths),
        )
        .await;
        return Err(format!("编辑消息持久化失败：{error}"));
    }

    cleanup_unreferenced_draft_attachments(
        state.inner(),
        raw_attachments_for_paths(replaced_attachment_candidates),
    )
    .await;
    let _ = state
        .with_state(|core_state| {
            if core_state.has_pending_turn_for(&session_id)
                && core_state.active_session_id() == session_id
            {
                core_state.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
                core_state.store.runtime.run.summary = "正在重新发送编辑后的消息".to_string();
                core_state.store.runtime.run.last_session_id = Some(session_id.clone());
                core_state.store.runtime.run.updated_at = tiangong_core::session::now_text();
            }
            Ok(())
        })
        .await;
    if ensured.is_new {
        start_stream_consumer(app, sid, stream_rx, ensured.instance_token);
    }

    Ok(())
}

fn validate_editable_message(
    session: &tiangong_core::session::Session,
    message_id: &str,
    base_content: &[tiangong_types::ContentBlock],
) -> Result<usize, anyhow::Error> {
    let message_index = session
        .messages
        .iter()
        .position(|message| message.id == message_id)
        .ok_or_else(|| anyhow::anyhow!("消息不存在：{message_id}"))?;
    let message = &session.messages[message_index];
    if message.role != tiangong_core::session::MessageRole::User {
        return Err(anyhow::anyhow!("只能编辑用户消息"));
    }
    if message.compact || message_index < session.summary_up_to {
        return Err(anyhow::anyhow!("该消息已被压缩或清空，无法编辑"));
    }
    if message.content != base_content {
        return Err(anyhow::anyhow!("消息已被更新，请基于最新内容重新编辑"));
    }
    Ok(message_index)
}

async fn restore_edited_session(
    state: &TiangongApp,
    session_id: &str,
    original_session: tiangong_core::session::Session,
    originally_pending: bool,
) {
    let _ = state
        .with_state(|core_state| {
            if let Some(session) = core_state
                .sessions_mut()
                .iter_mut()
                .find(|session| session.id == session_id)
            {
                *session = original_session;
            }
            if !originally_pending {
                core_state.clear_pending_turn_for(session_id);
            }
            core_state.persist_session_and_app(session_id)?;
            Ok(())
        })
        .await;
}
#[tauri::command]
pub async fn cancel_turn(state: State<'_, TiangongApp>) -> Result<bool, String> {
    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
        .await?;
    let session_lock = state.session_send_lock(&session_id);
    Ok(cancel_after_session_send_boundary(session_lock, || state.cancel_core(&session_id)).await)
}

async fn cancel_after_session_send_boundary(
    session_lock: Arc<tokio::sync::Mutex<()>>,
    cancel: impl FnOnce() -> bool,
) -> bool {
    // 发送准备阶段尚未安装 Core。等待同一会话的投递边界后再查找实例，确保取消
    // 命中本次刚创建的 Core，而不是在附件归档期间静默丢失。
    let _send_guard = session_lock.lock_owned().await;
    cancel()
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
    state: &TiangongApp,
) -> Result<bool, String> {
    use std::sync::mpsc;

    state.sync_core_config_from_state().await?;
    loop {
        let expected_session_id = state
            .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
            .await?;
        let session_lock = state.session_send_lock(&expected_session_id);
        let _send_guard = session_lock.lock_owned().await;
        if state.remote_turn_owner(&expected_session_id).is_some() {
            return Err("目标会话正在处理远端请求，暂时不能修改上下文".to_string());
        }
        let prepared = state
            .with_state(|core_state| {
                let idx = core_state.ensure_active_session_index();
                let session_id = core_state.sessions()[idx].id.clone();
                if session_id != expected_session_id {
                    return Ok(None);
                }
                let mut session_snapshot = core_state.sessions()[idx].clone();
                if session_snapshot.cwd.trim().is_empty() {
                    session_snapshot.cwd = core_state.workspace_dir().to_string();
                }
                if !core_state.has_pending_turn_for(&session_id) {
                    core_state.persist_session_and_app(&session_id)?;
                }
                Ok(Some((session_id, session_snapshot)))
            })
            .await?;
        let Some((session_id, session_snapshot)) = prepared else {
            continue;
        };

        let (stream_tx, stream_rx) = mpsc::channel::<tiangong_types::SessionStreamEvent>();
        let ensured = state
            .ensure_core(&session_id, session_snapshot, stream_tx)
            .await;
        if ensured.is_new {
            start_stream_consumer(
                app.clone(),
                ensured.session_id.clone(),
                stream_rx,
                ensured.instance_token.clone(),
            );
        }
        let input = match command {
            ContextSlashCommand::Compress => AgentInputKind::compress_context(),
            ContextSlashCommand::Reset => AgentInputKind::reset_context(),
        };
        return Ok(state.deliver_to_core_if_current(
            &ensured.session_id,
            &ensured.instance_token,
            input,
        ));
    }
}

/// 手动触发上下文压缩
#[tauri::command]
pub async fn compress_context(
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<bool, String> {
    run_context_slash_command(ContextSlashCommand::Compress, app, state.inner()).await
}

/// 清理上下文（重置 LLM 上下文到初始 system prompt）
#[tauri::command]
pub async fn reset_context(app: AppHandle, state: State<'_, TiangongApp>) -> Result<bool, String> {
    run_context_slash_command(ContextSlashCommand::Reset, app, state.inner()).await
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
    attachments: Vec<tiangong_media_archive::RawAttachment>,
    revision: u64,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<bool, String> {
    if session_id.trim().is_empty() {
        return Err("当前会话 ID 不能为空".to_string());
    }

    let is_running = state
        .with_state_read(|core_state| Ok(core_state.has_pending_turn_for(&session_id)))
        .await?;
    if !is_running {
        abort_session_send(state.inner(), &session_id, revision, false).await;
        return Ok(false);
    }

    send_message_inner(
        UserMessageDeliveryRequest {
            session_id,
            content,
            attachments,
            revision,
            delivery_kind: UserMessageDeliveryKind::Append,
            requires_draft_claim: true,
        },
        app,
        state.inner(),
    )
    .await?;
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
pub async fn get_trust_mode(
    session_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    state
        .with_state_read(|core_state| {
            let target_id = session_id
                .as_deref()
                .unwrap_or_else(|| core_state.active_session_id());
            let mode = core_state
                .sessions()
                .iter()
                .find(|session| session.id == target_id)
                .map(|session| session.trust_mode)
                .unwrap_or(core_state.agent_config().default_trust_mode);
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
pub async fn set_trust_mode(
    mode: String,
    session_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let trust_mode: tiangong_core::permission::TrustMode =
        serde_json::from_value(serde_json::Value::String(mode))
            .map_err(|e| format!("无效的信任模式: {e}"))?;

    let session_id = match session_id {
        Some(session_id) => session_id,
        None => {
            state
                .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
                .await?
        }
    };
    let session_lock = state.session_send_lock(&session_id);
    let _send_guard = session_lock.lock_owned().await;

    let previous_mode = state
        .with_state(|core_state| {
            let previous_mode = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .map(|session| session.trust_mode)
                .ok_or_else(|| anyhow::anyhow!("会话不存在，无法设置信任模式：{session_id}"))?;
            core_state.set_session_trust_mode_in_memory(&session_id, trust_mode)?;
            if let Err(error) = core_state.persist_app_only() {
                let _ = core_state.set_session_trust_mode_in_memory(&session_id, previous_mode);
                return Err(error);
            }
            Ok(previous_mode)
        })
        .await?;

    if let Err(error) = state.sync_core_config_from_state().await {
        rollback_session_trust_mode(state.inner(), &session_id, previous_mode, false).await;
        return Err(error);
    }
    // 配置替换命令可能排在当前 turn 后面，信任模式句柄必须立即生效。
    state.set_core_trust_mode(&session_id, trust_mode);

    let update = SessionMetadataUpdate {
        trust_mode: Some(trust_mode),
        ..SessionMetadataUpdate::default()
    };
    let receipt = match state.enqueue_session_metadata_update_if_live(&session_id, update) {
        Ok(receipt) => receipt,
        Err(error) => {
            rollback_session_trust_mode(state.inner(), &session_id, previous_mode, false).await;
            return Err(error);
        }
    };
    let persist_without_core = receipt.is_none();
    let persisted = if let Some(receipt) = receipt {
        receipt
            .await_persisted()
            .await
            .map_err(|error| error.to_string())
    } else {
        state
            .with_state(|core_state| core_state.persist_session_and_app(&session_id))
            .await
    };
    if let Err(error) = persisted {
        rollback_session_trust_mode(
            state.inner(),
            &session_id,
            previous_mode,
            persist_without_core,
        )
        .await;
        return Err(error);
    }
    Ok(())
}

async fn rollback_session_trust_mode(
    state: &TiangongApp,
    session_id: &str,
    previous_mode: tiangong_core::permission::TrustMode,
    persist_without_core: bool,
) {
    let rollback = state
        .with_state(|core_state| {
            core_state.set_session_trust_mode_in_memory(session_id, previous_mode)?;
            if persist_without_core {
                core_state.persist_session_and_app(session_id)
            } else {
                core_state.persist_app_only()
            }
        })
        .await;
    if let Err(error) = rollback {
        warn!(%error, %session_id, "回滚会话信任模式失败");
    }
    if let Err(error) = state.sync_core_config_from_state().await {
        warn!(%error, %session_id, "回滚会话信任模式后同步配置失败");
    }
    state.set_core_trust_mode(session_id, previous_mode);
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
pub async fn get_reasoning_effort(
    session_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    state
        .with_state_read(|core_state| {
            let target_id = session_id
                .as_deref()
                .unwrap_or_else(|| core_state.active_session_id());
            let effort = core_state
                .sessions()
                .iter()
                .find(|session| session.id == target_id)
                .and_then(|session| session.reasoning_effort.as_deref())
                .map(str::trim)
                .filter(|effort| !effort.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| core_state.agent_config().reasoning_effort.clone());
            Ok(effort)
        })
        .await
}

#[tauri::command]
pub async fn set_reasoning_effort(
    effort: String,
    session_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let valid = ["none", "low", "medium", "high", "max"];
    if !valid.contains(&effort.as_str()) {
        return Err(format!(
            "无效的思考强度: {effort}，可选值: {}",
            valid.join("/")
        ));
    }
    let session_id = match session_id {
        Some(session_id) => session_id,
        None => {
            state
                .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))
                .await?
        }
    };
    let session_lock = state.session_send_lock(&session_id);
    let _send_guard = session_lock.lock_owned().await;

    let (previous_override, previous_compatibility_value) = state
        .with_state(|core_state| {
            let previous_override = core_state
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .ok_or_else(|| anyhow::anyhow!("会话不存在，无法设置思考强度：{session_id}"))?
                .reasoning_effort
                .clone();
            let previous_compatibility_value = core_state.agent_config().reasoning_effort.clone();
            core_state.set_session_reasoning_effort_in_memory(&session_id, effort.clone())?;
            if let Err(error) = core_state.persist_app_only() {
                let _ = restore_session_reasoning_effort_in_memory(
                    core_state,
                    &session_id,
                    previous_override.clone(),
                    previous_compatibility_value.clone(),
                );
                return Err(error);
            }
            Ok((previous_override, previous_compatibility_value))
        })
        .await?;

    if let Err(error) = state.sync_core_config_from_state().await {
        rollback_session_reasoning_effort(
            state.inner(),
            &session_id,
            previous_override,
            previous_compatibility_value,
            false,
        )
        .await;
        return Err(error);
    }

    let update = SessionMetadataUpdate {
        reasoning_effort: Some(Some(effort)),
        ..SessionMetadataUpdate::default()
    };
    let receipt = match state.enqueue_session_metadata_update_if_live(&session_id, update) {
        Ok(receipt) => receipt,
        Err(error) => {
            rollback_session_reasoning_effort(
                state.inner(),
                &session_id,
                previous_override,
                previous_compatibility_value,
                false,
            )
            .await;
            return Err(error);
        }
    };
    let persist_without_core = receipt.is_none();
    let persisted = if let Some(receipt) = receipt {
        receipt
            .await_persisted()
            .await
            .map_err(|error| error.to_string())
    } else {
        state
            .with_state(|core_state| core_state.persist_session_and_app(&session_id))
            .await
    };
    if let Err(error) = persisted {
        rollback_session_reasoning_effort(
            state.inner(),
            &session_id,
            previous_override,
            previous_compatibility_value,
            persist_without_core,
        )
        .await;
        return Err(error);
    }
    Ok(())
}

fn restore_session_reasoning_effort_in_memory(
    core_state: &mut tiangong_app_state::app_state::TiangongState,
    session_id: &str,
    previous_override: Option<String>,
    previous_compatibility_value: String,
) -> anyhow::Result<()> {
    core_state.set_session_reasoning_effort_in_memory(
        session_id,
        previous_override
            .clone()
            .unwrap_or(previous_compatibility_value),
    )?;
    if previous_override.is_none() {
        let session = core_state
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow::anyhow!("会话不存在，无法回滚思考强度：{session_id}"))?;
        session.reasoning_effort = None;
    }
    Ok(())
}

async fn rollback_session_reasoning_effort(
    state: &TiangongApp,
    session_id: &str,
    previous_override: Option<String>,
    previous_compatibility_value: String,
    persist_without_core: bool,
) {
    let rollback = state
        .with_state(|core_state| {
            restore_session_reasoning_effort_in_memory(
                core_state,
                session_id,
                previous_override,
                previous_compatibility_value,
            )?;
            if persist_without_core {
                core_state.persist_session_and_app(session_id)
            } else {
                core_state.persist_app_only()
            }
        })
        .await;
    if let Err(error) = rollback {
        warn!(%error, %session_id, "回滚会话思考强度失败");
    }
    if let Err(error) = state.sync_core_config_from_state().await {
        warn!(%error, %session_id, "回滚会话思考强度后同步配置失败");
    }
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
                tiangong_llm::models_config::ModelsConfig::resolve_api_key(&provider.api_key);
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
    let reg = tiangong_plugin_task::task_registry();
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
    let reg = tiangong_plugin_task::task_registry();
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
    use tiangong_llm::models_config::ModelCapability;

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
    // Skill + MCP servers + active tools 均由各自 plugin 自管，先读取快照。
    let skills = state.skill_plugin.installed_skills();
    let mcp_servers = state.mcp_plugin.mcp_servers();
    let active_tools = state.mcp_plugin.cached_active_tools();
    let mut candidates = Vec::new();

    // 已启用的 Skill（数据来自 skill plugin）
    for skill in &skills {
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

    // 已启用的 MCP 服务器（数据来自 mcp plugin）
    for server in &mcp_servers {
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
}

/// 获取运行状态快照
#[tauri::command]
pub async fn get_run_snapshot(state: State<'_, TiangongApp>) -> Result<RunSnapshotView, String> {
    state
        .with_state_read(|core_state| {
            Ok(build_full_snapshot_with_status(
                core_state,
                active_session_is_executing(core_state),
            ))
        })
        .await
}

/// 获取输入草稿
#[tauri::command]
pub async fn get_input_draft(
    session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_app_state::app_state::SessionInputDraft, String> {
    if session_id.trim().is_empty() {
        return Err("草稿会话 ID 不能为空".to_string());
    }
    let resolved_session_id = state.resolve_draft_session_id(&session_id);
    state
        .with_state_read(|core_state| Ok(core_state.session_input_draft(&resolved_session_id)))
        .await
}

/// 设置输入草稿
#[tauri::command]
pub async fn set_input_draft(
    session_id: String,
    mut draft: tiangong_app_state::app_state::SessionInputDraft,
    claim_revision: Option<u64>,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_app_state::app_state::SessionInputDraft, String> {
    if session_id.trim().is_empty() {
        return Err("草稿会话 ID 不能为空".to_string());
    }
    // 迁移可能恰好与一个已发出的旧 key 写入并发。获锁后再解析一次
    // redirect，若目标变化则重新在新 key 的锁上排队。
    let requested_session_id = session_id;
    let (session_id, draft_guard) = loop {
        if state.draft_was_discarded(&requested_session_id) {
            return Err("该草稿已被丢弃".to_string());
        }
        let resolved = state.resolve_draft_session_id(&requested_session_id);
        let guard = state.draft_update_lock(&resolved).lock_owned().await;
        if state.draft_was_discarded(&requested_session_id) {
            return Err("该草稿已被丢弃".to_string());
        }
        let confirmed = state.resolve_draft_session_id(&requested_session_id);
        if confirmed == resolved {
            break (resolved, guard);
        }
        drop(guard);
    };
    if session_id != requested_session_id {
        let exists = state
            .with_state_read(|core_state| {
                Ok(core_state
                    .sessions()
                    .iter()
                    .any(|session| session.id == session_id))
            })
            .await?;
        if !exists {
            return Err("草稿所属会话已删除".to_string());
        }
    }
    let current = state
        .with_state_read(|core_state| Ok(core_state.session_input_draft(&session_id)))
        .await?;
    if draft.revision < current.revision {
        return Ok(current);
    }

    let old_attachments = current.attachments.clone();
    let mut transaction = None;
    if same_draft_attachment_selection(&draft.attachments, &current.attachments) {
        draft.attachments = current.attachments;
    } else if !draft.attachments.is_empty() {
        let raw = std::mem::take(&mut draft.attachments);
        let staged = tokio::task::spawn_blocking(move || {
            tiangong_media_archive::AttachmentStore::default().store_batch(raw)
        })
        .await
        .map_err(|error| format!("草稿附件保存任务失败：{error}"))?
        .map_err(|error| format!("草稿附件保存失败：{error}"))?;
        draft.attachments = staged
            .stored()
            .iter()
            .map(|attachment| tiangong_media_archive::RawAttachment {
                kind: attachment.kind,
                source: attachment.local_path.clone(),
                mime_type: Some(attachment.mime_type.clone()),
                original_name: Some(attachment.original_name.clone()),
            })
            .collect();
        transaction = Some(staged);
    }

    let (persisted, applied) = state
        .with_state(|core_state| {
            core_state.set_session_input_draft_with_outcome(&session_id, draft)
        })
        .await?;
    if applied {
        if let Some(transaction) = transaction.take() {
            transaction.commit();
        }
    }
    // stale 写入未被采用时，transaction 在此作用域结束自动回滚新归档文件。
    drop(transaction);

    let mut cleanup_candidates = if applied { old_attachments } else { Vec::new() };
    let mut claim_error = None;
    if let Some(revision) = claim_revision {
        if persisted.revision != revision {
            claim_error = Some(format!(
                "草稿已在发送前更新（发送 revision={revision}，当前 revision={}）",
                persisted.revision
            ));
        } else {
            let attachment_paths = persisted
                .attachments
                .iter()
                .map(|attachment| attachment.source.clone())
                .collect();
            match state.register_draft_send_claim(&session_id, revision, attachment_paths) {
                Ok(replaced_paths) => {
                    cleanup_candidates.extend(raw_attachments_for_paths(replaced_paths));
                }
                Err(error) => claim_error = Some(error),
            }
        }
    }
    // 草稿与租约已落盘/登记，清理等待 send lock 前释放 draft lock，
    // 使慢发送期间的 R+1/R+2 新输入仍能按会话串行并立即持久化。
    drop(draft_guard);
    // 草稿状态已先落盘；文件清理再等待该会话发送事务结束，避免用户在慢发送期间
    // 删除附件时把正在投递的稳定文件提前移除。
    let cleanup_lock = state.session_send_lock(&session_id);
    let _cleanup_guard = cleanup_lock.lock().await;
    cleanup_unreferenced_draft_attachments(state.inner(), cleanup_candidates).await;
    if let Some(error) = claim_error {
        return Err(error);
    }
    Ok(persisted)
}

fn same_draft_attachment_selection(
    incoming: &[tiangong_media_archive::RawAttachment],
    current: &[tiangong_media_archive::RawAttachment],
) -> bool {
    incoming == current
}

pub(crate) async fn cleanup_unreferenced_draft_attachments(
    state: &TiangongApp,
    candidates: Vec<tiangong_media_archive::RawAttachment>,
) {
    if candidates.is_empty() {
        return;
    }
    let claimed_paths = state.claimed_draft_attachment_paths();
    let referenced = state
        .with_state_read(|core_state| {
            let mut paths = claimed_paths;
            for draft in core_state.store.session.input_drafts.values() {
                paths.extend(draft.attachments.iter().map(|item| item.source.clone()));
            }
            for session in core_state.sessions() {
                for message in &session.messages {
                    paths.extend(
                        message
                            .extract_media_assets()
                            .into_iter()
                            .map(|item| item.url),
                    );
                }
            }
            Ok(paths)
        })
        .await
        .unwrap_or_default();

    let mut checked = std::collections::HashSet::new();
    for candidate in candidates {
        if !checked.insert(candidate.source.clone()) {
            continue;
        }
        if referenced.contains(&candidate.source)
            || !tiangong_media_archive::is_archived_media_path_any(&candidate.source)
        {
            continue;
        }
        if let Err(error) = std::fs::remove_file(&candidate.source) {
            tracing::warn!(path = %candidate.source, error = %error, "清理未引用草稿附件失败");
        }
    }
}

pub(crate) fn raw_attachments_for_paths(
    paths: Vec<String>,
) -> Vec<tiangong_media_archive::RawAttachment> {
    paths
        .into_iter()
        .map(|source| tiangong_media_archive::RawAttachment {
            kind: tiangong_types::MediaKind::File,
            source,
            mime_type: None,
            original_name: None,
        })
        .collect()
}

pub(crate) fn session_attachment_candidates(
    session: &tiangong_core::session::Session,
) -> Vec<tiangong_media_archive::RawAttachment> {
    let paths = session
        .messages
        .iter()
        .flat_map(|message| {
            message
                .extract_media_assets()
                .into_iter()
                .map(|asset| asset.url)
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    raw_attachments_for_paths(paths)
}

/// 为尚未落盘的新会话生成稳定 SCRU128 草稿 ID。
#[tauri::command]
pub fn new_draft_id() -> String {
    scru128::new().to_string()
}

/// 草稿会话创建真实 Session 后迁移全部文字、附件和 revision。
#[tauri::command]
pub async fn migrate_input_draft(
    from_session_id: String,
    to_session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_app_state::app_state::SessionInputDraft, String> {
    if from_session_id.trim().is_empty() || to_session_id.trim().is_empty() {
        return Err("草稿会话 ID 和目标会话 ID 不能为空".to_string());
    }
    if from_session_id == to_session_id {
        return state
            .with_state_read(|core_state| Ok(core_state.session_input_draft(&to_session_id)))
            .await;
    }

    // 所有双 key 锁均按 ID 排序：draft locks -> send locks -> app state。
    // 这样可以与 set_input_draft 的锁顺序保持一致。
    let mut ids = [from_session_id.clone(), to_session_id.clone()];
    ids.sort();
    let _first_draft_guard = state.draft_update_lock(&ids[0]).lock_owned().await;
    let _second_draft_guard = state.draft_update_lock(&ids[1]).lock_owned().await;
    let _first_send_guard = state.session_send_lock(&ids[0]).lock_owned().await;
    let _second_send_guard = state.session_send_lock(&ids[1]).lock_owned().await;
    if state.draft_was_discarded(&from_session_id) {
        return Err("源草稿已被丢弃，无法迁移".to_string());
    }

    let migrated = state
        .with_state(|core_state| {
            core_state.migrate_session_input_draft(&from_session_id, &to_session_id)
        })
        .await?;
    // 在锁仍持有时发布 redirect；之后拿到旧 key 锁的迟到写入会复查并
    // 改投真实 session_id。运行期不删除旧 Arc 锁，避免新旧锁并存。
    state.redirect_input_draft(&from_session_id, &to_session_id);
    Ok(migrated)
}

#[tauri::command]
pub async fn remove_input_draft(
    session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let draft_lock = state.draft_update_lock(&session_id);
    let _draft_guard = draft_lock.lock_owned().await;
    let send_lock = state.session_send_lock(&session_id);
    let _send_guard = send_lock.lock_owned().await;
    let mut attachments = state
        .with_state_read(|core_state| Ok(core_state.session_input_draft(&session_id).attachments))
        .await?;
    state
        .with_state(|core_state| core_state.remove_session_input_draft(&session_id))
        .await?;
    attachments.extend(raw_attachments_for_paths(
        state.release_any_draft_send_claim(&session_id),
    ));
    state.mark_draft_discarded(&session_id);
    cleanup_unreferenced_draft_attachments(state.inner(), attachments).await;
    Ok(())
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

/// 设置指定会话的工作目录
#[tauri::command]
pub async fn set_session_cwd(
    session_id: String,
    cwd: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("会话 ID 不能为空".to_string());
    }
    let path = std::path::Path::new(&cwd);
    if !path.is_dir() {
        return Err(format!("路径不存在或不是目录：{cwd}"));
    }
    state
        .with_state(|core_state| core_state.update_session_cwd(&session_id, cwd.clone()))
        .await?;

    // 只同步目标会话 Core，不受此时活动会话切换影响。
    {
        let cores = state.cores.lock().map_err(|e| e.to_string())?;
        if let Some(core) = cores.get(&session_id) {
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
    Ok(state
        .mcp_plugin
        .mcp_servers()
        .iter()
        .map(McpServerView::from_core)
        .collect())
}

/// 获取 MCP 服务器健康状态
#[tauri::command]
pub async fn get_mcp_health(
    state: State<'_, TiangongApp>,
) -> Result<Vec<serde_json::Value>, String> {
    let statuses = state.mcp_plugin.mcp_server_health_statuses();
    statuses
        .into_iter()
        .map(|s| serde_json::to_value(s).map_err(|e| e.to_string()))
        .collect()
}

/// 探测单个 MCP 服务器（按 name），写回健康缓存。供前端添加/编辑/重试后刷新该行。
#[tauri::command]
pub async fn probe_mcp_server(name: String, state: State<'_, TiangongApp>) -> Result<(), String> {
    state
        .mcp_plugin
        .probe_mcp_server(&name)
        .map_err(|e| e.to_string())
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
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    use tiangong_plugin_mcp::{
        McpTransportMode, RegisterMcpServerOptions, RegisterMcpServerRequest,
    };

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

    let header_vec = headers.unwrap_or_default().into_iter().collect();
    let env_vec = env.unwrap_or_default().into_iter().collect();
    let request = RegisterMcpServerRequest {
        name,
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
        },
    };
    // MCP 管理由 mcp plugin 自治（读写 ~/.tiangong/mcp.json），不经 TiangongState。
    // core engine rebuild 由 plugin 的 on_engine_rebuilt 钩子触发 capability 重配。
    let message = state
        .mcp_plugin
        .register_mcp_server(request)
        .map_err(|e| e.to_string())?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

/// 编辑 MCP 服务器（按 name 定位，name 自身不可改）
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn update_mcp_server(
    name: String,
    command: String,
    args: Vec<String>,
    transport: Option<String>,
    endpoint: Option<String>,
    auth_header: Option<String>,
    headers: Option<std::collections::HashMap<String, String>>,
    env: Option<std::collections::HashMap<String, String>>,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    use tiangong_plugin_mcp::{
        McpTransportMode, RegisterMcpServerOptions, RegisterMcpServerRequest,
    };

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

    let header_vec = headers.unwrap_or_default().into_iter().collect();
    let env_vec = env.unwrap_or_default().into_iter().collect();
    let request = RegisterMcpServerRequest {
        name: name.clone(),
        command,
        args,
        tags: vec![],
        // enabled 由列表开关单独控制，编辑表单不覆盖；update_mcp_server 会保留原值
        enabled: true,
        options: RegisterMcpServerOptions {
            transport,
            endpoint,
            auth_header,
            headers: header_vec,
            env: env_vec,
        },
    };
    let message = state
        .mcp_plugin
        .update_mcp_server(&name, request)
        .map_err(|e| e.to_string())?;
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
        .mcp_plugin
        .remove_mcp_server(&name)
        .map_err(|e| e.to_string())?;
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
        .mcp_plugin
        .set_mcp_server_enabled(&name, enabled)
        .map_err(|e| e.to_string())?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

// ============================================================================
// Skill 管理
// ============================================================================

/// 获取已安装的 Skill 列表
#[tauri::command]
pub async fn get_skills(state: State<'_, TiangongApp>) -> Result<Vec<SkillView>, String> {
    Ok(state
        .skill_plugin
        .installed_skills()
        .iter()
        .map(SkillView::from_core)
        .collect())
}

/// 刷新 Skill 注册表（重扫 skills/<id>/）
#[tauri::command]
pub async fn refresh_skills(state: State<'_, TiangongApp>) -> Result<String, String> {
    let message = state
        .skill_plugin
        .refresh_skills()
        .map_err(|e| e.to_string())?;
    state.sync_core_config_from_state().await?;
    Ok(message)
}

/// 获取 Skill 完整详情（按需读取 SKILL.md）
#[tauri::command]
pub async fn get_skill_detail(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<SkillDetailView, String> {
    let detail = state
        .skill_plugin
        .get_skill_detail(&id)
        .map_err(|e| e.to_string())?;
    Ok(SkillDetailView::from_core(&detail))
}

/// 移除 Skill
#[tauri::command]
pub async fn remove_skill(id: String, state: State<'_, TiangongApp>) -> Result<String, String> {
    let outcome = state
        .skill_plugin
        .remove_skill(&id)
        .map_err(|e| e.to_string())?;
    // 清理 plugin 报告的孤儿托管 MCP server（MCP 配置由 mcp plugin 自管）
    if !outcome.orphan_mcp_servers.is_empty() {
        for orphan in &outcome.orphan_mcp_servers {
            let _ = state.mcp_plugin.remove_mcp_server(orphan);
        }
    }
    state.sync_core_config_from_state().await?;
    Ok(outcome.message)
}

/// 获取 Skill 的环境变量（合并 skill.toml 声明的 requires.env + .env.local 已有值）
#[tauri::command]
pub async fn get_skill_env(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let installed = state.skill_plugin.installed_skills();
    let skill = installed
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("未找到 skill：{id}"))?;
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
}

/// 设置 Skill 的环境变量
#[tauri::command]
pub async fn set_skill_env(
    id: String,
    env: std::collections::HashMap<String, String>,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let installed = state.skill_plugin.installed_skills();
    let skill = installed
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("未找到 skill：{id}"))?;
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
            .map_err(|e| format!("写入 .env.local 失败：{e}"))?;
    }
    Ok(())
}

/// 设置 Skill 启用状态
#[tauri::command]
pub async fn set_skill_enabled(
    id: String,
    enabled: bool,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let message = state
        .skill_plugin
        .set_skill_enabled(&id, enabled)
        .map_err(|e| e.to_string())?;
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
    let config = tiangong_memory::registry::load_memory_config();
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
    tiangong_memory::registry::save_memory_config(memory_config).map_err(|err| err.to_string())?;
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
    _state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_memory::MemoryNode>, String> {
    tiangong_memory::gui_api::list_memory_nodes_for_gui(query, status, limit, offset)
        .await
        .map_err(|err| err.to_string())
}

/// 统计全部记忆节点真实总数。
#[tauri::command]
pub async fn count_memory_nodes(
    query: Option<String>,
    status: Option<String>,
    created_after: Option<String>,
    _state: State<'_, TiangongApp>,
) -> Result<usize, String> {
    tiangong_memory::gui_api::count_memory_nodes_for_gui(query, status, created_after)
        .await
        .map_err(|err| err.to_string())
}

/// 手动新增或调整一条记忆。
#[tauri::command]
pub async fn upsert_manual_memory(
    draft: tiangong_memory::ManualMemoryDraft,
    _state: State<'_, TiangongApp>,
) -> Result<tiangong_memory::MemoryNode, String> {
    if draft.title.trim().is_empty() {
        return Err("记忆标题不能为空".to_string());
    }
    if draft.summary.trim().is_empty() {
        return Err("记忆内容不能为空".to_string());
    }
    tiangong_memory::gui_api::upsert_manual_memory_for_gui(draft)
        .await
        .map_err(|err| err.to_string())
}

/// 归档或恢复记忆节点。
#[tauri::command]
pub async fn set_memory_node_status(
    node_id: String,
    status: String,
    _state: State<'_, TiangongApp>,
) -> Result<(), String> {
    tiangong_memory::gui_api::set_memory_node_status_for_gui(node_id, status)
        .await
        .map_err(|err| err.to_string())
}

/// 列出指定记忆节点的图关系。
#[tauri::command]
pub async fn list_memory_relations(
    node_id: String,
    _state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_memory::MemoryRelation>, String> {
    tiangong_memory::gui_api::list_memory_relations_for_gui(node_id)
        .await
        .map_err(|err| err.to_string())
}

/// 批量列出多个记忆节点的关联关系（去重，修复 N+1 性能问题）。
#[tauri::command]
pub async fn list_memory_relations_batch(
    node_ids: Vec<String>,
    _state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_memory::MemoryRelation>, String> {
    tiangong_memory::gui_api::list_memory_relations_batch_for_gui(node_ids)
        .await
        .map_err(|err| err.to_string())
}

/// 新增或调整记忆图关系。
#[tauri::command]
pub async fn upsert_memory_relation(
    draft: tiangong_memory::MemoryRelationDraft,
    _state: State<'_, TiangongApp>,
) -> Result<tiangong_memory::MemoryRelation, String> {
    tiangong_memory::gui_api::upsert_memory_relation_for_gui(draft)
        .await
        .map_err(|err| err.to_string())
}

/// 删除记忆图关系。
#[tauri::command]
pub async fn delete_memory_relation(
    relation_id: String,
    _state: State<'_, TiangongApp>,
) -> Result<(), String> {
    tiangong_memory::gui_api::delete_memory_relation_for_gui(relation_id)
        .await
        .map_err(|err| err.to_string())
}

/// 手动测试记忆召回，不写入会话消息链。
#[tauri::command]
pub async fn test_memory_recall(
    query: String,
    limit: Option<usize>,
    _state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_memory::RecallHit>, String> {
    tiangong_memory::gui_api::test_memory_recall_for_gui(query, limit)
        .await
        .map_err(|err| err.to_string())
}

// ── 索引管理 ──

/// 列出所有 Workspace 索引
#[tauri::command]
pub async fn list_workspace_indexes(
) -> Result<Vec<tiangong_plugin_index::WorkspaceIndexInfo>, String> {
    tiangong_plugin_index::list_workspace_indexes_for_gui().map_err(|err| err.to_string())
}

/// 删除指定 Workspace 索引
#[tauri::command]
pub async fn delete_workspace_index(workspace_id: String) -> Result<(), String> {
    tiangong_plugin_index::delete_workspace_index_for_gui(&workspace_id)
        .map_err(|err| err.to_string())
}

/// 重建指定路径的 Workspace 索引
#[tauri::command]
pub async fn rebuild_workspace_index(root: String) -> Result<usize, String> {
    let root = std::path::PathBuf::from(&root);
    tiangong_plugin_index::rebuild_workspace_index_for_gui(&root).map_err(|err| err.to_string())
}

/// 获取所有可用的模型能力列表
#[tauri::command]
pub async fn get_model_capabilities() -> Result<Vec<ModelCapabilityInfo>, String> {
    use tiangong_llm::models_config::ModelCapability;

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
    use tiangong_core::model::{ProviderProtocol, SingleProviderClient};
    use tiangong_llm::models_config::ModelsConfig;
    use tiangong_llm::ModelEndpoint;

    let resolved_key = ModelsConfig::resolve_api_key(&api_key);
    let endpoint = ModelEndpoint {
        base_url,
        api_key: resolved_key,
        model: String::new(),
        protocol: protocol
            .as_deref()
            .and_then(|value| value.parse::<ProviderProtocol>().ok())
            .unwrap_or_default(),
        timeout_ms: timeout_ms.unwrap_or(60_000),
        options: serde_json::Value::Object(serde_json::Map::new()),
    };
    SingleProviderClient::list_models_async(&endpoint)
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
    use tiangong_llm::models_config::ModelsConfig;

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
    let store = tiangong_scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
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
    let store = tiangong_scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
    let now = chrono::Local::now().naive_local().to_string();
    let job = tiangong_scheduler::model::Job {
        id: scru128::new().to_string(),
        name,
        description,
        trigger_type: tiangong_scheduler::model::TriggerType::Cron,
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
    let store = tiangong_scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
    let req = tiangong_scheduler::model::UpdateJobRequest {
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
    let store = tiangong_scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
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
    let store = tiangong_scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
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
    let store = tiangong_scheduler::store::JobStore::open().map_err(|e| e.to_string())?;
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
    let store =
        tiangong_scheduler::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
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
    let store =
        tiangong_scheduler::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
    let now = chrono::Local::now().naive_local().to_string();
    let webhook = tiangong_scheduler::webhook::model::Webhook {
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
    let store =
        tiangong_scheduler::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
    let req = tiangong_scheduler::webhook::model::UpdateWebhookRequest {
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
    let store =
        tiangong_scheduler::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
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
    let store =
        tiangong_scheduler::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
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
    let store =
        tiangong_scheduler::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::{
        append_tool_result_message, cancel_after_session_send_boundary,
        done_event_keeps_turn_running,
    };

    #[tokio::test]
    async fn cancel_waits_for_send_boundary_and_returns_delivery_result() {
        let session_lock = Arc::new(tokio::sync::Mutex::new(()));
        let send_guard = session_lock.clone().lock_owned().await;
        let delivered = Arc::new(AtomicBool::new(false));
        let delivered_in_cancel = delivered.clone();
        let mut cancel = tokio::spawn(async move {
            cancel_after_session_send_boundary(session_lock, || {
                delivered_in_cancel.store(true, Ordering::Release);
                true
            })
            .await
        });

        tokio::task::yield_now().await;
        assert!(!cancel.is_finished(), "发送边界释放前取消必须等待");
        assert!(!delivered.load(Ordering::Acquire));
        drop(send_guard);

        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut cancel)
                .await
                .expect("发送边界释放后取消应继续")
                .expect("取消任务不应 panic")
        );
        assert!(delivered.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cancel_reports_when_no_core_accepts_the_command() {
        let session_lock = Arc::new(tokio::sync::Mutex::new(()));
        assert!(!cancel_after_session_send_boundary(session_lock, || false).await);
    }

    #[test]
    fn empty_done_keeps_pending_turn_running() {
        let event = tiangong_types::StreamEvent::Done { usage: None };
        assert!(done_event_keeps_turn_running(&event, true));
        assert!(!done_event_keeps_turn_running(&event, false));
    }

    #[test]
    fn final_done_with_usage_keeps_queued_next_turn_running() {
        let event = tiangong_types::StreamEvent::Done {
            usage: Some(tiangong_types::TokenUsage::default()),
        };
        assert!(done_event_keeps_turn_running(&event, true));
    }

    #[test]
    fn error_event_never_keeps_turn_running() {
        let event = tiangong_types::StreamEvent::Error {
            message: "failed".to_string(),
        };
        assert!(!done_event_keeps_turn_running(&event, true));
    }

    #[test]
    fn tool_result_deduplication_is_scoped_to_the_latest_call_batch() {
        use tiangong_core::session::{Message, MessageRole, MessageToolCall, Session};

        let mut session = Session::new("desktop-reused-tool-id");
        for (round, result) in [("first", "old"), ("second", "new")] {
            if round == "second" {
                session.append_message(MessageRole::User, "next");
            }
            let mut assistant = Message::new(MessageRole::Assistant, String::new());
            assistant.tool_calls = vec![MessageToolCall {
                id: "reused".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({}),
            }];
            session.messages.push(assistant);
            append_tool_result_message(
                &mut session,
                Some("reused"),
                "read_file",
                result.to_string(),
                false,
            );
        }

        let results = session
            .messages
            .iter()
            .filter(|message| message.tool_call_id.as_deref() == Some("reused"))
            .map(Message::text_content)
            .collect::<Vec<_>>();
        assert_eq!(results, vec!["old", "new"]);
    }
}

/// 按模型名从 context_windows.json 映射表解析默认 context_window（token 数）。
/// 供前端在编辑模型时预填默认值。
#[tauri::command]
pub async fn resolve_model_context_window(model: String) -> Result<usize, String> {
    let dir = tiangong_config::io::storage_root();
    Ok(tiangong_config::io::resolve_context_limit_at(&dir, &model))
}
