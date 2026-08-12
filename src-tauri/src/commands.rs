use crate::app::TiangongApp;
use crate::view::*;
use base64::{engine::general_purpose, Engine as _};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, Window};
use tauri_plugin_notification::{NotificationExt, PermissionState};
use tiangong_core::agent_input::AgentInputKind;
use tracing::warn;

use crate::workspace_tabs::{
    WorkspaceTabKind as TabKind, WorkspaceTabRef, WorkspaceTabState as TabState,
};

const MAX_ATTACHMENT_BASE64_BYTES: u64 = 50 * 1024 * 1024;

use tiangong_toolkit::configure_tokio_no_window;

#[allow(dead_code)]
fn done_event_keeps_turn_running(
    event: &tiangong_types::StreamEvent,
    has_pending_turn: bool,
) -> bool {
    matches!(event, tiangong_types::StreamEvent::Done { .. }) && has_pending_turn
}

fn merge_agent_worker_messages(
    messages: &mut Vec<tiangong_types::Message>,
    cached: &[tiangong_types::Message],
) {
    let mut indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            let worker_id = message.worker_id.as_deref()?;
            worker_id
                .starts_with("agent:")
                .then(|| ((worker_id.to_string(), message.id.clone()), index))
        })
        .collect::<std::collections::HashMap<_, _>>();

    for message in cached {
        let Some(worker_id) = message
            .worker_id
            .as_deref()
            .filter(|worker_id| worker_id.starts_with("agent:"))
        else {
            continue;
        };
        let key = (worker_id.to_string(), message.id.clone());
        if let Some(index) = indices.get(&key).copied() {
            messages[index] = message.clone();
        } else {
            let index = messages.len();
            messages.push(message.clone());
            indices.insert(key, index);
        }
    }
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
        .with_state_read(|state| Ok(state.active_session_id.as_str() == session_id))
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
    core_state.config.models.has_capability(capability)
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
    let manager = state.core_manager.clone();
    tokio::task::spawn_blocking(move || {
        manager
            .list_session_metadata()
            .iter()
            .filter(|metadata| metadata.parent_session_id.is_none())
            .map(SessionListItem::from_metadata)
            .collect()
    })
    .await
    .map_err(|error| format!("等待会话列表加载失败：{error}"))
}

/// 获取单个会话的元数据（精确更新列表时使用，避免全量刷新）。
#[tauri::command]
pub async fn get_session_meta(
    session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<Option<SessionListItem>, String> {
    let manager = state.core_manager.clone();
    tokio::task::spawn_blocking(move || {
        let meta = tiangong_core_manager::SessionMetadata::load_from_storage(
            manager.storage_root(),
            &session_id,
        )
        .ok()
        .filter(|m| m.parent_session_id.is_none());
        Ok(meta.as_ref().map(SessionListItem::from_metadata))
    })
    .await
    .map_err(|error| format!("读取会话元数据失败：{error}"))?
}

/// 获取指定会话的统一工作区 Tab 元数据
#[tauri::command]
pub async fn get_session_tabs(
    session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<SessionTabsView, String> {
    let manager = state.core_manager.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<SessionTabsView> {
        if !manager.session_exists(&session_id) {
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
        let terminal =
            tiangong_plugin_terminal::session_store::TerminalSessionStore::load_or_migrate_legacy(
                &session_id,
            )?;
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
        if let Err(error) =
            crate::workspace_tabs::save_layout(&session_id, &tabs, active_tab_id.as_deref())
        {
            warn!(%error, session_id, "清理工作区标签页布局失败");
        }

        Ok(SessionTabsView {
            tabs,
            active_tab_id,
        })
    })
    .await
    .map_err(|error| format!("等待会话标签页加载失败：{error}"))?
    .map_err(|error| error.to_string())
}

/// 写入指定会话的统一工作区 Tab 元数据
#[tauri::command]
pub async fn set_session_tabs(
    session_id: String,
    tabs: Vec<TabState>,
    active_tab_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let manager = state.core_manager.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        if !manager.session_exists(&session_id) {
            return Err(anyhow::anyhow!("会话不存在：{session_id}"));
        }
        crate::workspace_tabs::save_layout(&session_id, &tabs, active_tab_id.as_deref())
    })
    .await
    .map_err(|error| format!("等待会话标签页保存失败：{error}"))?
    .map_err(|error| error.to_string())
}

/// 切换到指定会话
#[tauri::command]
pub async fn switch_session(
    app: AppHandle,
    session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    if !state.inner().core_manager.session_exists(&session_id) {
        return Err(format!("会话不存在：{session_id}"));
    }

    if !state.core_manager.has_live_core(&session_id) {
        let (stream_tx, stream_rx) = std::sync::mpsc::channel::<tiangong_types::StreamEvent>();
        let ensured = state
            .ensure_core(&session_id, None, None, None, stream_tx)
            .await;
        if ensured.is_new {
            start_stream_consumer(app, ensured.session_id, stream_rx);
        }
    }

    state
        .with_state(|core_state| {
            core_state.active_session_id = session_id;
            Ok(())
        })
        .await?;

    Ok(())
}

/// 从 Session 真相源加载指定会话的界面数据。
#[tauri::command]
pub async fn load_session(
    session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<crate::view::LoadedSessionView, String> {
    let config = state.core_manager.config().snapshot();
    let context_limit = if config.context_limit == 0 {
        tiangong_core::core_config::default_context_limit()
    } else {
        config.context_limit
    };
    let default_reasoning_effort = config.reasoning_effort.clone();
    let manager = state.core_manager.clone();
    let session_id_for_load = session_id.clone();
    let session = tokio::task::spawn_blocking(move || manager.load_session(&session_id_for_load))
        .await
        .map_err(|error| format!("等待会话加载失败：{error}"))??;

    let mut view = crate::view::LoadedSessionView::from_session(
        &session,
        context_limit,
        &default_reasoning_effort,
    );
    merge_agent_worker_messages(
        &mut view.messages,
        &state.agent_worker_view_messages(&session_id),
    );
    Ok(view)
}

/// 删除指定会话（逻辑删除）。
///
/// 会话文件原子移动到 `trash/sessions/`，从列表立即消失。
/// 不做媒体/teams/PTY/browser 清理——那些留给配置页的物理删除。
#[tauri::command]
pub async fn delete_session(
    session_id: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let deleted_id = session_id;
    let _cache_guard = state
        .input_cache_update_lock(&deleted_id)
        .lock_owned()
        .await;
    let _send_guard = state.session_send_lock(&deleted_id).lock_owned().await;
    // 逻辑删除：原子移动到 trash + 取消 Core。
    state
        .inner()
        .core_manager
        .delete_session(&deleted_id)
        .await
        .map_err(|error| format!("删除会话失败：{error}"))?;
    // 清理内存状态。
    state.fail_remote_session_waiters(&deleted_id, "目标会话已删除");
    state
        .with_state(|core_state| {
            crate::session_ops::remove_session_state(core_state, &state.core_manager, &deleted_id);
            Ok(())
        })
        .await?;
    let _ = state.release_any_input_send_claim(&deleted_id);
    state.clear_agent_worker_view(&deleted_id);
    state.remove_session_send_lock(&deleted_id);
    Ok(())
}

/// 删除指定 workspace（cwd）下的所有会话（逻辑删除）。
#[tauri::command]
pub async fn delete_sessions_by_cwd(
    cwd: String,
    state: State<'_, TiangongApp>,
) -> Result<DeleteResult, String> {
    let mut deleted_ids = state
        .with_state_read(|core_state| {
            let ids: Vec<String> = core_state
                .core_manager
                .list_session_metadata()
                .iter()
                .filter(|metadata| metadata.cwd == cwd)
                .map(|metadata| metadata.id.clone())
                .collect();
            Ok::<_, anyhow::Error>(ids)
        })
        .await?;
    deleted_ids.sort();
    let mut input_cache_guards = Vec::with_capacity(deleted_ids.len());
    for id in &deleted_ids {
        input_cache_guards.push(state.input_cache_update_lock(id).lock_owned().await);
    }
    let mut send_guards = Vec::with_capacity(deleted_ids.len());
    for id in &deleted_ids {
        send_guards.push(state.session_send_lock(id).lock_owned().await);
    }
    // 并发逻辑删除（等待 worker 退出 + 原子移动到 trash）。收集每项结果。
    let core_manager = state.inner().core_manager.clone();
    let deletes = deleted_ids.iter().map(|id| {
        let id = id.clone();
        let manager = core_manager.clone();
        async move {
            let result = manager.delete_session(&id).await;
            (id, result)
        }
    });
    let results = futures_util::future::join_all(deletes).await;
    let (succeeded, failed): (Vec<_>, Vec<_>) =
        results.into_iter().partition(|(_, result)| result.is_ok());
    let succeeded_ids: Vec<String> = succeeded.into_iter().map(|(id, _)| id).collect();
    let failed_ids: Vec<String> = failed
        .into_iter()
        .map(|(id, result)| {
            tracing::warn!(session_id = %id, error = ?result.err(), "批量删除：该会话删除失败");
            id
        })
        .collect();
    // 只清理成功删除的会话的内存状态。
    state
        .with_state(|core_state| {
            for id in &succeeded_ids {
                state.fail_remote_session_waiters(id, "目标会话已删除");
                crate::session_ops::remove_session_state(core_state, &state.core_manager, id);
            }
            Ok(())
        })
        .await?;
    for id in &succeeded_ids {
        let _ = state.release_any_input_send_claim(id);
        state.clear_agent_worker_view(id);
        state.remove_session_send_lock(id);
    }
    drop(send_guards);
    drop(input_cache_guards);
    if !failed_ids.is_empty() {
        tracing::warn!(
            succeeded = succeeded_ids.len(),
            failed = failed_ids.len(),
            "批量删除部分失败"
        );
    }
    Ok(DeleteResult {
        succeeded: succeeded_ids,
        failed: failed_ids,
    })
}

/// 失败回滚只能关闭本次 ensure 绑定的实例，并且必须等待 worker 最终写盘结束，
/// 才能恢复宿主快照，避免旧 Core 的迟到持久化覆盖回滚结果。
pub(crate) async fn shutdown_join_core_if_current(state: &TiangongApp, session_id: &str) {
    if let Err(error) = state.core_manager.retire_core(session_id, false).await {
        tracing::warn!(%session_id, %error, "失败回滚时关闭 Core 失败");
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
        .with_state_read(|core_state| Ok(core_state.active_session_id.as_str().to_string()))
        .await?;
    let session_lock = state.session_send_lock(&session_id);
    let _send_guard = session_lock.lock_owned().await;
    let active_session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id.clone()))
        .await?;
    if active_session_id != session_id {
        return Err("活动会话已切换，请重新修改标题".to_string());
    }
    // 统一经 CoreManager（有 live Core 时委托 Core 走 turn 安全写入，否则直写盘）。
    // 标题变更通知由 Core 统一发 TitleChanged 事件，前端据此更新，不再 emit sessions_updated。
    state
        .core_manager
        .set_core_title(&session_id, title, false)?;
    Ok(())
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
    workspace_dir: Option<String>,
    initial_trust_mode: Option<tiangong_types::TrustMode>,
    initial_reasoning_effort: Option<String>,
    delivery_kind: UserMessageDeliveryKind,
    requires_input_claim: bool,
}

/// 发送消息并执行
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn send_message(
    session_id: String,
    content: String,
    attachments: Vec<tiangong_media_archive::RawAttachment>,
    revision: u64,
    cwd: Option<String>,
    trust_mode: Option<tiangong_types::TrustMode>,
    reasoning_effort: Option<String>,
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
            workspace_dir: cwd,
            initial_trust_mode: trust_mode,
            initial_reasoning_effort: reasoning_effort,
            delivery_kind: UserMessageDeliveryKind::NewTurn,
            requires_input_claim: true,
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
            let session_id = core_state.active_session_id.as_str().to_string();
            let revision = crate::state_ops::input_cache(core_state, &session_id).revision;
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
            workspace_dir: None,
            initial_trust_mode: None,
            initial_reasoning_effort: None,
            delivery_kind: UserMessageDeliveryKind::NewTurn,
            requires_input_claim: false,
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
    use tiangong_types::StreamEvent;

    let UserMessageDeliveryRequest {
        session_id,
        content,
        attachments,
        revision,
        workspace_dir,
        initial_trust_mode,
        initial_reasoning_effort,
        delivery_kind,
        requires_input_claim,
    } = request;

    if session_id.trim().is_empty() {
        return Err("目标会话 ID 不能为空".to_string());
    }

    if let Some(command) = parse_context_slash_command(&content) {
        let active_id = state
            .with_state_read(|core_state| Ok(core_state.active_session_id.as_str().to_string()))
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

    if requires_input_claim && !state.has_input_send_claim(&session_id, revision) {
        abort_session_send(state, &session_id, revision, false).await;
        return Err("发送输入尚未冻结，请基于最新输入重试".to_string());
    }

    let has_pending_turn = state
        .with_state_read(|core_state| {
            Ok(crate::state_ops::has_pending_turn(core_state, &session_id))
        })
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
    if state.input_revision_was_delivered(&session_id, revision) {
        abort_session_send(state, &session_id, revision, false).await;
        return Err("该版本输入已成功发送，已拒绝重复投递".to_string());
    }

    // 注：发送消息不改变任何配置（trust_mode/reasoning_effort/model 由用户在设置页
    // 手动变更，那些路径自行调用 sync_core_config_from_state）。ensure_core 内部对既有
    // Core 已做 replace_config（ensure.rs），无需在此全量加载所有 session metadata。
    if let Err(error) = state
        .with_state(|core_state| {
            crate::state_ops::begin_input_send(core_state, &session_id, revision)
        })
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

    // 附件所有权从临时事务转移给该消息。
    let created_paths = transaction
        .newly_created_paths()
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    transaction.commit();

    // 获取或创建 TiangongCore(Core 的 deliver 负责 load session + 追加消息 +
    // persist,host 不再预写——issue #245 整改方案)。
    let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
    let workspace_dir = if state.core_manager.session_exists(&session_id) {
        None
    } else {
        let cwd = match workspace_dir {
            Some(cwd) if !cwd.trim().is_empty() => cwd,
            _ => {
                state
                    .with_state_read(|core_state| Ok(core_state.workspace_dir.clone()))
                    .await?
            }
        };
        if !std::path::Path::new(&cwd).is_dir() {
            abort_session_send(state, &session_id, revision, true).await;
            return Err(format!("新对话工作目录不存在或不是目录：{cwd}"));
        }
        Some(cwd)
    };
    let ensured = state
        .ensure_core(
            &session_id,
            workspace_dir,
            initial_trust_mode,
            initial_reasoning_effort,
            stream_tx,
        )
        .await;
    let sid = ensured.session_id.clone();
    if let Err(error) =
        state.deliver_prepared_if_live(&sid, user_message_id.clone(), prepared.clone())
    {
        shutdown_join_core_if_current(state, &sid).await;
        let _ = restore_failed_user_message_state(state, &session_id, &user_message_id).await;
        cleanup_unreferenced_input_attachments(state, raw_attachments_for_paths(created_paths))
            .await;
        abort_session_send(state, &session_id, revision, true).await;
        return Err(format!("消息投递失败：{error}"));
    }

    // 消息已投递给 Core（Core 内部会持久化），附件从此由消息引用持有，不能再自动回滚。
    state.mark_input_revision_delivered(&session_id, revision);
    let finish_result = state
        .with_state(|core_state| {
            let cache =
                crate::state_ops::finish_input_send(core_state, &session_id, revision, true)?;
            Ok(cache)
        })
        .await;
    if let Err(error) = finish_result {
        // Core 已确认稳定消息，此时不能把已发送的消息误报为可重试失败。
        tracing::error!(session_id, revision, error = %error, "消息已发送，但输入缓存终态更新失败");
    }
    release_input_send_claim_and_cleanup(state, &session_id, revision).await;

    if ensured.is_new {
        start_stream_consumer(app, sid, stream_rx);
    }

    Ok(())
}

async fn abort_session_send(state: &TiangongApp, session_id: &str, revision: u64, began: bool) {
    if began {
        let _ = state
            .with_state(|core_state| {
                crate::state_ops::finish_input_send(core_state, session_id, revision, false)
            })
            .await;
    }
    release_input_send_claim_and_cleanup(state, session_id, revision).await;
}

async fn release_input_send_claim_and_cleanup(
    state: &TiangongApp,
    session_id: &str,
    revision: u64,
) {
    let paths = state.release_input_send_claim(session_id, revision);
    if !paths.is_empty() {
        cleanup_unreferenced_input_attachments(state, raw_attachments_for_paths(paths)).await;
    }
}

pub(crate) async fn restore_failed_user_message_state(
    state: &TiangongApp,
    session_id: &str,
    message_id: &str,
) -> Result<(), String> {
    state
        .with_state(|core_state| {
            if core_state
                .core_manager
                .list_session_metadata()
                .iter()
                .any(|m| m.id == session_id)
            {
                crate::session_ops::remove_failed_message(
                    &state.core_manager,
                    session_id,
                    message_id,
                )?;
                crate::state_ops::remove_pending_message(core_state, session_id, message_id);
                Ok(())
            } else {
                Err(anyhow::anyhow!("目标会话已不存在：{session_id}"))
            }
        })
        .await
}

pub(crate) async fn attachment_capability_snapshot(
    state: &TiangongApp,
) -> Result<tiangong_media_archive::AttachmentCapabilitySnapshot, String> {
    state
        .with_state_read(|core_state| {
            use tiangong_llm::ModelCapability;
            let models = &core_state.config.models;
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

fn emit_session_stream_event(
    app: &AppHandle,
    session_id: &str,
    event: &tiangong_types::StreamEvent,
) {
    let _ = app.emit(
        "stream_event",
        &tiangong_types::SessionEvent {
            session_id: session_id.to_string(),
            event: event.clone(),
        },
    );
}

/// 消费 StreamEvent：按会话转发给前端，并维护消息投递边界。
pub(crate) fn start_stream_consumer(
    app: AppHandle,
    session_id: String,
    stream_rx: std::sync::mpsc::Receiver<tiangong_types::StreamEvent>,
) {
    use tiangong_types::StreamEvent;

    let rt = tokio::runtime::Handle::current();
    thread::spawn(move || {
        for session_event in stream_rx.iter() {
            let app_state = app.state::<TiangongApp>();
            if matches!(&session_event, StreamEvent::TurnElapsed { .. }) {
                if app_state.core_manager.has_live_core(&session_id) {
                    emit_session_stream_event(&app, &session_id, &session_event);
                }
                continue;
            }

            // 标题变更：通知类事件，不触碰消息投递临界区，只转发流事件。
            // 前端收到 title_changed 后直接更新内存中对应会话标题，无需整表刷新。
            if matches!(&session_event, StreamEvent::TitleChanged { .. }) {
                emit_session_stream_event(&app, &session_id, &session_event);
                continue;
            }

            let event_lock = app_state.session_send_lock(&session_id);
            let _event_guard = rt.block_on(event_lock.lock_owned());
            if !app_state.core_manager.has_live_core(&session_id) {
                // 已退役 Core 的缓冲事件不得修改同会话新实例的 pending、消息或 UI。
                continue;
            }

            let terminal_event = matches!(
                &session_event,
                StreamEvent::Done { .. } | StreamEvent::Error { .. }
            );
            // 普通流事件立即转发；终态等运行状态收尾后再对外发布。
            if !terminal_event {
                emit_session_stream_event(&app, &session_id, &session_event);
            }

            let sid = session_id.clone();
            let event = session_event;
            let is_done = matches!(event, StreamEvent::Done { .. });
            let is_error = matches!(event, StreamEvent::Error { .. });
            if let StreamEvent::AgentOutput {
                agent_id,
                agent_role,
                agent_label,
                messages,
            } = &event
            {
                app_state.merge_agent_output_view(
                    &sid,
                    agent_id,
                    agent_role,
                    agent_label,
                    messages,
                );
            }

            // App 只维护投递边界；正文、Thinking、工具和计时事件不触碰应用状态锁。
            let accepted_message_id = match &event {
                StreamEvent::UserMessage { message_id, .. } => Some(message_id.clone()),
                _ => None,
            };
            if accepted_message_id.is_some() || is_done || is_error {
                let _ = rt.block_on(app.state::<TiangongApp>().with_state(|core_state| {
                    if let Some(message_id) = accepted_message_id {
                        crate::state_ops::accept_pending_message(core_state, &sid, &message_id);
                    }
                    if is_done || is_error {
                        crate::state_ops::complete_accepted_turn(core_state, &sid);
                    }
                    Ok(())
                }));
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
                let final_sid = sid.clone();
                let completed_remote_message_id = app_state.remote_turn_owner(&final_sid);

                if let Some(message_id) = completed_remote_message_id {
                    rt.block_on(crate::embedded_server::complete_remote_turn_from_stream(
                        app.state::<TiangongApp>().inner(),
                        &final_sid,
                        &message_id,
                    ));
                }

                emit_session_stream_event(&app, &final_sid, &event);
                // 精确通知：该会话的元数据已更新（消息数/时间），前端只更新这一条。
                let _ = app.emit("session_meta_updated", &final_sid);
                // 标题生成已在 send_message_inner 投递后并行启动（与 chat turn 同时跑），
                // 完成后经 Command::SetTitle 投递给 turn task 安全写入，此处不再串行处理。
                // 不 break — 消费线程继续运行，等待下一轮消息的 StreamEvent
            }
        }

        let app_state = app.state::<TiangongApp>();
        let eof_lock = app_state.session_send_lock(&session_id);
        let _eof_guard = rt.block_on(eof_lock.lock_owned());
        // 通道关闭说明持有该发送端的 Core 已被显式移除。若同会话已经创建了新 Core，
        // 这是旧消费者的正常退出，不能按 session_id 清理新实例或覆盖新一轮状态。
        if !app_state.core_manager.has_live_core(&session_id) {
            let _ = rt.block_on(app_state.with_state(|core_state| {
                crate::state_ops::clear_pending_turn(core_state, &session_id);
                Ok(())
            }));
            app_state.fail_remote_session_waiters(&session_id, "执行已中断：Core 事件流已关闭");
            emit_session_stream_event(
                &app,
                &session_id,
                &tiangong_types::StreamEvent::Error {
                    message: "Core 事件流已关闭".to_string(),
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
        .with_state_read(|core_state| {
            Ok(crate::state_ops::has_pending_turn(core_state, &session_id))
        })
        .await?;
    if has_pending_turn {
        return Err("目标会话正在执行，暂时不能编辑重发".to_string());
    }
    // 注：编辑重发不改变配置；ensure_core 内部对既有 Core 已做 replace_config，
    // 无需在此全量加载所有 session metadata（与 send_message 同理）。

    // 第一遍只读校验发生在任何附件 IO 之前。
    state
        .with_state_read(|core_state| {
            // 校验需读 messages（完整 Session 字段）。会话存在性用 metadata 判定，
            // 校验本身用从磁盘 load 的最新 session（issue #245：真相源归磁盘）。
            if !core_state
                .core_manager
                .list_session_metadata()
                .iter()
                .any(|m| m.id == session_id)
            {
                return Err(anyhow::anyhow!("会话不存在：{session_id}"));
            }
            if crate::state_ops::has_pending_turn(core_state, &session_id) {
                return Err(anyhow::anyhow!("目标会话正在执行，暂时不能编辑重发"));
            }
            Ok(())
        })
        .await?;
    // 从磁盘 load 校验（不在 app-state 锁内做文件 IO）。
    let session_for_validation = state.inner().core_manager.load_session(&session_id)?;
    validate_editable_message(&session_for_validation, &message_id, &base_content)
        .map_err(|error| error.to_string())?;

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
    let (original_session, session_snapshot) = state
        .with_state(|core_state| {
            let (original, runtime) = crate::session_ops::edit_prepared_user_message(
                core_state,
                &state.core_manager,
                &session_id,
                &message_id,
                prepared_for_state,
            )?;
            Ok((original, runtime))
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

    // 最终校验及稳定消息落盘均成功后，才终止旧 Core（cancel + take + join）。
    // 旧 Core 的最终收尾可能最后一次写回编辑前快照；join 后重新落盘当前编辑状态，
    // 此后已无旧写入者可覆盖。
    state
        .inner()
        .core_manager
        .retire_core(&session_id, true)
        .await?;
    crate::session_ops::restore_session(session_snapshot.clone())
        .map_err(|error| error.to_string())?;

    let (stream_tx, stream_rx) = mpsc::channel::<tiangong_types::StreamEvent>();
    let ensured = state
        .ensure_core(&session_id, None, None, None, stream_tx)
        .await;
    let sid = ensured.session_id.clone();
    if let Err(error) = state.deliver_prepared_if_live(&sid, message_id.clone(), prepared.clone()) {
        shutdown_join_core_if_current(state.inner(), &sid).await;
        restore_edited_session(state.inner(), &session_id, original_session).await;
        cleanup_unreferenced_input_attachments(
            state.inner(),
            raw_attachments_for_paths(created_paths.clone()),
        )
        .await;
        return Err(format!("编辑消息投递失败：{error}"));
    }

    cleanup_unreferenced_input_attachments(
        state.inner(),
        raw_attachments_for_paths(replaced_attachment_candidates),
    )
    .await;
    if ensured.is_new {
        start_stream_consumer(app, sid, stream_rx);
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
    if message.phase == tiangong_core::session::MessagePhase::CompressedResume {
        return Err(anyhow::anyhow!("该消息为压缩恢复消息，无法编辑"));
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
) {
    let _ = state
        .with_state(|core_state| {
            crate::session_ops::restore_session(original_session)?;
            crate::state_ops::clear_pending_turn(core_state, session_id);
            Ok(())
        })
        .await;
}
#[tauri::command]
pub async fn cancel_turn(state: State<'_, TiangongApp>) -> Result<bool, String> {
    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id.as_str().to_string()))
        .await?;
    let session_lock = state.session_send_lock(&session_id);
    Ok(cancel_after_session_send_boundary(session_lock, || {
        state.inner().core_manager.cancel_core(&session_id)
    })
    .await)
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

    // 注：compress/reset 不改变配置；ensure_core 内部对既有 Core 已做 replace_config，
    // 无需在此全量加载所有 session metadata（与 send_message 同理）。
    loop {
        let expected_session_id = state
            .with_state_read(|core_state| Ok(core_state.active_session_id.as_str().to_string()))
            .await?;
        let session_lock = state.session_send_lock(&expected_session_id);
        let _send_guard = session_lock.lock_owned().await;
        if state.remote_turn_owner(&expected_session_id).is_some() {
            return Err("目标会话正在处理远端请求，暂时不能修改上下文".to_string());
        }
        let current_session_id = state
            .with_state_read(|core_state| Ok(core_state.active_session_id.clone()))
            .await?;
        if current_session_id != expected_session_id {
            continue;
        }
        let session_id = current_session_id;
        let (stream_tx, stream_rx) = mpsc::channel::<tiangong_types::StreamEvent>();
        let ensured = state
            .ensure_core(&session_id, None, None, None, stream_tx)
            .await;
        if ensured.is_new {
            start_stream_consumer(app.clone(), ensured.session_id.clone(), stream_rx);
        }
        let input = match command {
            ContextSlashCommand::Compress => AgentInputKind::compress_context(),
            ContextSlashCommand::Reset => AgentInputKind::reset_context(),
        };
        return Ok(state
            .core_manager
            .deliver_to_core_if_live(&ensured.session_id, input));
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
        .with_state_read(|core_state| Ok(core_state.active_session_id.as_str().to_string()))
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
        .with_state_read(|core_state| {
            Ok(crate::state_ops::has_pending_turn(core_state, &session_id))
        })
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
            workspace_dir: None,
            initial_trust_mode: None,
            initial_reasoning_effort: None,
            delivery_kind: UserMessageDeliveryKind::Append,
            requires_input_claim: true,
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
        .with_state_read(|core_state| Ok(core_state.active_session_id.as_str().to_string()))
        .await?;
    state
        .inner()
        .core_manager
        .respond_approval_to_core(&session_id, request_id, approved);
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
                .unwrap_or(core_state.active_session_id.as_str());
            let mode = core_state
                .core_manager
                .list_session_metadata()
                .iter()
                .find(|m| m.id == target_id)
                .map(|m| m.trust_mode)
                .unwrap_or(core_state.config.default_trust_mode);
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
                .with_state_read(|core_state| Ok(core_state.active_session_id.as_str().to_string()))
                .await?
        }
    };
    let session_lock = state.session_send_lock(&session_id);
    let _send_guard = session_lock.lock_owned().await;

    let previous_mode =
        crate::session_ops::update_trust_mode(&state.core_manager, &session_id, trust_mode)
            .map_err(|error| error.to_string())?;

    if let Err(error) = state.sync_core_config_from_state().await {
        rollback_session_trust_mode(state.inner(), &session_id, previous_mode).await;
        return Err(error);
    }
    // 配置替换命令可能排在当前 turn 后面，信任模式句柄必须立即生效。
    state
        .inner()
        .core_manager
        .set_core_trust_mode(&session_id, trust_mode);
    // Session 中的 trust_mode 不在这里单独落盘，由当前 turn 或下一轮统一持久化。
    Ok(())
}

async fn rollback_session_trust_mode(
    state: &TiangongApp,
    session_id: &str,
    previous_mode: tiangong_core::permission::TrustMode,
) {
    if let Err(error) =
        crate::session_ops::update_trust_mode(&state.core_manager, session_id, previous_mode)
    {
        warn!(%error, %session_id, "回滚会话信任模式失败");
    }
    if let Err(error) = state.sync_core_config_from_state().await {
        warn!(%error, %session_id, "回滚会话信任模式后同步配置失败");
    }
    state
        .core_manager
        .set_core_trust_mode(session_id, previous_mode);
}

#[tauri::command]
pub async fn get_default_trust_mode(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| {
            let mode = core_state.config.default_trust_mode;
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

    let mut config = state
        .with_state_read(|core_state| Ok(core_state.config.clone()))
        .await?;
    config.default_trust_mode = trust_mode;
    tiangong_config::registry::update(config.clone()).map_err(|error| error.to_string())?;
    state
        .with_state(|core_state| {
            core_state.config = config;
            core_state.agent_config.default_trust_mode = trust_mode;
            Ok(())
        })
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(())
}

#[tauri::command]
pub async fn get_reasoning_effort(
    session_id: Option<String>,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let (target_id, default_effort) = state
        .with_state_read(|core_state| {
            Ok((
                session_id.unwrap_or_else(|| core_state.active_session_id.clone()),
                core_state.agent_config.reasoning_effort.clone(),
            ))
        })
        .await?;
    Ok(state
        .inner()
        .core_manager
        .load_session(&target_id)
        .ok()
        .and_then(|session| session.reasoning_effort)
        .map(|effort| effort.trim().to_string())
        .filter(|effort| !effort.is_empty())
        .unwrap_or(default_effort))
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
                .with_state_read(|core_state| Ok(core_state.active_session_id.as_str().to_string()))
                .await?
        }
    };
    let session_lock = state.session_send_lock(&session_id);
    let _send_guard = session_lock.lock_owned().await;

    let next_effort = effort.clone();
    let previous_override =
        crate::session_ops::update_reasoning_effort(&state.core_manager, &session_id, Some(effort))
            .map_err(|error| error.to_string())?;

    // 先通知当前 turn，确保工具执行后的下一次模型请求立即使用新强度。
    state
        .inner()
        .core_manager
        .set_core_reasoning_effort(&session_id, next_effort);
    if let Err(error) = state.sync_core_config_from_state().await {
        rollback_session_reasoning_effort(state.inner(), &session_id, previous_override).await;
        return Err(error);
    }
    Ok(())
}

async fn rollback_session_reasoning_effort(
    state: &TiangongApp,
    session_id: &str,
    previous_override: Option<String>,
) {
    if let Err(error) = crate::session_ops::update_reasoning_effort(
        &state.core_manager,
        session_id,
        previous_override,
    ) {
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
            let models = &core_state.config.models;
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
    // task_records 是完整 Session 字段，session 真相源归磁盘（issue #245），
    // 经 CoreManager 从磁盘加载，不在 app-state 的内存镜像里读。
    let sid = state
        .with_state_read(|core_state| {
            Ok::<String, anyhow::Error>(
                session_id
                    .as_deref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| core_state.active_session_id.as_str().to_string()),
            )
        })
        .await?;
    match state.inner().core_manager.load_session(&sid) {
        Ok(session) => {
            let cost = tiangong_core::observe::build_session_cost(sid, &session.task_records);
            Ok(serde_json::to_value(cost).unwrap_or_default())
        }
        Err(_) => Ok(serde_json::json!({})),
    }
}

/// 获取当前活跃的 Worker 列表
#[tauri::command]
pub async fn list_workers(state: State<'_, TiangongApp>) -> Result<Vec<serde_json::Value>, String> {
    state.with_state_read(|_core_state| Ok(Vec::new())).await
}

/// 语音合成：将文本转换为音频，返回 base64 编码的音频数据
#[tauri::command]
pub async fn synthesize_speech(
    text: String,
    state: State<'_, TiangongApp>,
) -> Result<SpeechResult, String> {
    let models_config = state
        .with_state_read(|core_state| Ok(core_state.config.models.clone()))
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
                        .config
                        .models
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
        .with_state_read(|core_state| Ok(core_state.config.models.clone()))
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
        .with_state_read(|core_state| Ok(core_state.config.models.clone()))
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
        configure_tokio_no_window(&mut command);
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
        configure_tokio_no_window(&mut command);
        command
            .output()
            .await
            .map_err(|e| format!("播放失败：{e}"))?;
    }

    #[cfg(target_os = "linux")]
    {
        let mut command = tokio::process::Command::new("aplay");
        command.arg(&file_path);
        configure_tokio_no_window(&mut command);
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

/// 获取 @提及补全候选列表。
///
/// 经 Core 聚合（CoreManager::get_any_mentions 遍历任意活跃 Core 的全部插件，
/// 含 native 与 WASM 插件经 mention 标准接口贡献的候选）。原硬编码的 skill/mcp
/// 调用已迁入各自 WASM 插件的 mention-candidates 导出，host 不再区分插件类型。
/// 无活跃 Core 时返回空列表。
#[tauri::command]
pub async fn get_mention_candidates(
    state: State<'_, TiangongApp>,
) -> Result<Vec<MentionCandidate>, String> {
    Ok(state.core_manager.get_any_mentions())
}

/// 获取输入框缓存。
#[tauri::command]
pub async fn get_input_cache(
    cache_key: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_app_state::app_state::InputCache, String> {
    if cache_key.trim().is_empty() {
        return Err("输入缓存键不能为空".to_string());
    }
    state
        .with_state_read(|core_state| Ok(crate::state_ops::input_cache(core_state, &cache_key)))
        .await
}

/// 设置输入框缓存。
#[tauri::command]
pub async fn set_input_cache(
    cache_key: String,
    mut cache: tiangong_app_state::app_state::InputCache,
    claim_revision: Option<u64>,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_app_state::app_state::InputCache, String> {
    if cache_key.trim().is_empty() {
        return Err("输入缓存键不能为空".to_string());
    }
    if state.input_cache_was_discarded(&cache_key) {
        return Err("该输入缓存已被丢弃".to_string());
    }
    let cache_guard = state.input_cache_update_lock(&cache_key).lock_owned().await;
    if state.input_cache_was_discarded(&cache_key) {
        return Err("该输入缓存已被丢弃".to_string());
    }
    let current = state
        .with_state_read(|core_state| Ok(crate::state_ops::input_cache(core_state, &cache_key)))
        .await?;
    if cache.revision < current.revision {
        return Ok(current);
    }

    let old_attachments = current.attachments.clone();
    let mut transaction = None;
    if same_input_attachment_selection(&cache.attachments, &current.attachments) {
        cache.attachments = current.attachments;
    } else if !cache.attachments.is_empty() {
        let raw = std::mem::take(&mut cache.attachments);
        let staged = tokio::task::spawn_blocking(move || {
            tiangong_media_archive::AttachmentStore::default().store_batch(raw)
        })
        .await
        .map_err(|error| format!("输入附件保存任务失败：{error}"))?
        .map_err(|error| format!("输入附件保存失败：{error}"))?;
        cache.attachments = staged
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

    let (stored, applied) = state
        .with_state(|core_state| crate::state_ops::set_input_cache(core_state, &cache_key, cache))
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
        if stored.revision != revision {
            claim_error = Some(format!(
                "输入已在发送前更新（发送 revision={revision}，当前 revision={}）",
                stored.revision
            ));
        } else {
            let attachment_paths = stored
                .attachments
                .iter()
                .map(|attachment| attachment.source.clone())
                .collect();
            match state.register_input_send_claim(&cache_key, revision, attachment_paths) {
                Ok(replaced_paths) => {
                    cleanup_candidates.extend(raw_attachments_for_paths(replaced_paths));
                }
                Err(error) => claim_error = Some(error),
            }
        }
    }
    // 输入缓存与租约已更新/登记，清理等待 send lock 前释放缓存锁，
    // 使慢发送期间的 R+1/R+2 新输入仍能按会话串行并立即同步。
    drop(cache_guard);
    // 输入缓存已先更新；文件清理再等待该会话发送事务结束，避免用户在慢发送期间
    // 删除附件时把正在投递的稳定文件提前移除。
    let cleanup_lock = state.session_send_lock(&cache_key);
    let _cleanup_guard = cleanup_lock.lock().await;
    cleanup_unreferenced_input_attachments(state.inner(), cleanup_candidates).await;
    if let Some(error) = claim_error {
        return Err(error);
    }
    Ok(stored)
}

fn same_input_attachment_selection(
    incoming: &[tiangong_media_archive::RawAttachment],
    current: &[tiangong_media_archive::RawAttachment],
) -> bool {
    incoming == current
}

pub(crate) async fn cleanup_unreferenced_input_attachments(
    state: &TiangongApp,
    candidates: Vec<tiangong_media_archive::RawAttachment>,
) {
    if candidates.is_empty() {
        return;
    }
    let claimed_paths = state.claimed_input_attachment_paths();
    // 引用扫描需要遍历消息内容（完整 Session 字段）。session 真相源归磁盘
    //（issue #245）：先在锁外拿到会话 id 列表，再逐个从磁盘 load 扫描媒体引用，
    // 避免在 app-state 锁内做文件 IO。
    let session_ids: Vec<String> = state
        .with_state_read(|core_state| {
            Ok(core_state
                .core_manager
                .list_session_metadata()
                .iter()
                .map(|metadata| metadata.id.clone())
                .collect::<Vec<_>>())
        })
        .await
        .unwrap_or_default();
    let mut referenced = state
        .with_state_read(|core_state| {
            let mut paths = claimed_paths;
            for cache in core_state.input_caches.values() {
                paths.extend(cache.attachments.iter().map(|item| item.source.clone()));
            }
            Ok(paths)
        })
        .await
        .unwrap_or_default();

    // 从磁盘加载完整会话，扫描媒体引用（真相源归磁盘）。
    for session_id in &session_ids {
        let Ok(session) = state.core_manager.load_session(session_id) else {
            continue;
        };
        for message in &session.messages {
            referenced.extend(
                message
                    .extract_media_assets()
                    .into_iter()
                    .map(|item| item.url),
            );
        }
    }

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
            tracing::warn!(path = %candidate.source, error = %error, "清理未引用输入附件失败");
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

/// 为尚未落盘的新会话生成稳定 SCRU128 ID。这里只分配 ID，不创建 Session。
#[tauri::command]
pub fn new_session_id() -> String {
    scru128::new().to_string()
}

// ============================================================================
// 物理删除（配置页触发）
// ============================================================================

/// 批量删除结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeleteResult {
    /// 实际删除成功的会话 ID 列表。
    pub succeeded: Vec<String>,
    /// 删除失败的会话 ID 列表。
    pub failed: Vec<String>,
}

/// 回收区中的会话摘要（配置页展示用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrashedSession {
    pub id: String,
    pub title: String,
    pub message_count: usize,
    pub updated_at: String,
    /// 是否正在清理中（资源可能已部分删除，不可恢复）。
    pub purging: bool,
}

/// 物理清理进度事件 payload。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PurgeProgress {
    pub current: usize,
    pub total: usize,
    pub session_id: String,
    pub title: String,
    pub status: String, // "cleaning" | "done" | "error"
}

/// 从磁盘 JSON 读取回收区会话摘要（只读浅字段）。
fn read_trashed_summary(id: &str, path: &std::path::Path, purging: bool) -> TrashedSession {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .map(|value| TrashedSession {
            id: id.to_string(),
            title: value
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("（无标题）")
                .to_string(),
            message_count: value
                .get("messages")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter(|msg| msg.get("role").and_then(|r| r.as_str()) == Some("user"))
                        .count()
                })
                .unwrap_or(0),
            updated_at: value
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            purging,
        })
        .unwrap_or_else(|| TrashedSession {
            id: id.to_string(),
            title: "（无法读取）".to_string(),
            message_count: 0,
            updated_at: String::new(),
            purging,
        })
}

/// 扫描回收区，返回待物理清理的会话列表。
#[tauri::command]
pub async fn list_trashed_sessions(
    state: State<'_, TiangongApp>,
) -> Result<Vec<TrashedSession>, String> {
    let manager = state.core_manager.clone();
    tokio::task::spawn_blocking(move || {
        let storage_root = manager.storage_root();
        // 扫描回收区（可恢复）和正在清理（不可恢复）两个目录。
        let trashed_ids = manager.list_trashed_session_ids();
        let purging_ids = manager.list_purging_session_ids();
        let mut result = Vec::new();
        // 回收区会话（可恢复）
        for id in &trashed_ids {
            let path = storage_root
                .join("trash")
                .join("sessions")
                .join(format!("{id}.json"));
            result.push(read_trashed_summary(id, &path, false));
        }
        // 正在清理会话（不可恢复，资源可能已部分删除）
        for id in &purging_ids {
            let path = storage_root
                .join("trash")
                .join("purging")
                .join(format!("{id}.json"));
            result.push(read_trashed_summary(id, &path, true));
        }
        result.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(result)
    })
    .await
    .map_err(|error| format!("扫描回收区失败：{error}"))?
}

/// 从回收区恢复单个会话（trash/sessions/ → sessions/）。
#[tauri::command]
pub async fn restore_deleted_session(
    session_id: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    use tauri::Emitter;
    let manager = state.core_manager.clone();
    tokio::task::spawn_blocking(move || manager.restore_session_file(&session_id))
        .await
        .map_err(|error| format!("恢复任务失败：{error}"))??;
    // 恢复后会话重新出现在列表中，通知前端刷新。
    let _ = app.emit("sessions_updated", &());
    Ok(())
}

/// 发送清理失败进度事件。
fn emit_error(app: &AppHandle, index: usize, total: usize, session_id: &str) {
    use tauri::Emitter;
    let _ = app.emit(
        "purge_progress",
        PurgeProgress {
            current: index + 1,
            total,
            session_id: session_id.to_string(),
            title: String::new(),
            status: "error".to_string(),
        },
    );
}

/// 物理清理所有回收区会话（配置页"全部清理"触发）。
///
/// 逐个清理 media/teams/layout/PTY/browser/trash 文件，通过 `purge_progress`
/// 事件推送进度。已清理的会话不会重复清理。
#[tauri::command]
pub async fn purge_all_deleted_sessions(
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<usize, String> {
    use tauri::Emitter;

    let manager = state.core_manager.clone();
    // 合并待清理列表：回收区中的 + 上次清理失败残留在 purging 中的。
    let trashed_ids = manager.list_trashed_session_ids();
    let purging_ids = manager.list_purging_session_ids();
    let all_ids: Vec<String> = trashed_ids
        .iter()
        .chain(purging_ids.iter())
        .cloned()
        .collect();
    let total = all_ids.len();
    if total == 0 {
        return Ok(0);
    }

    let storage_root = manager.storage_root().to_path_buf();
    let media_root = storage_root.join("media");
    let app_handle = app.clone();

    let mut succeeded: Vec<String> = Vec::new();
    for (index, session_id) in all_ids.iter().enumerate() {
        let _ = app_handle.emit(
            "purge_progress",
            PurgeProgress {
                current: index,
                total,
                session_id: session_id.clone(),
                title: String::new(),
                status: "cleaning".to_string(),
            },
        );

        // 阶段 1：移到 purging（原子移动，标记"正在清理"）。
        // 已在 purging 中的（上次失败残留）跳过移动。
        if trashed_ids.contains(session_id) {
            if let Err(e) = manager.move_to_purging(session_id) {
                warn!(%e, session_id, "移动到 purging 失败");
                emit_error(&app_handle, index, total, session_id);
                continue;
            }
        }

        // 阶段 2：清理全部资源。
        let mut error_msg: Option<String> = None;

        // layout
        if let Err(e) = crate::workspace_tabs::remove_layout(session_id) {
            error_msg = Some(format!("删除布局失败：{e}"));
        }
        // PTY/browser
        if error_msg.is_none() {
            tiangong_plugin_terminal::destroy_session_pty(&app, session_id);
            if let Some(browser_state) =
                app.try_state::<tiangong_plugin_browser::BrowserPluginState>()
            {
                browser_state.registry.destroy_session(session_id);
            }
        }
        // media/teams（占用空间最大，必须删成功）
        if error_msg.is_none() {
            if let Err(e) = purge_session_resources(&storage_root, &media_root, session_id) {
                error_msg = Some(e);
            }
        }

        if let Some(msg) = error_msg {
            warn!(%msg, session_id, "物理清理失败，保留 purging 记录供重试");
            emit_error(&app_handle, index, total, session_id);
            continue;
        }

        // 阶段 3：全部资源清理成功 → 删 purging 记录。
        // 记录删除也是清理完成的一部分；失败时保留记录供下次重试，不能误报成功。
        if let Err(e) = manager.delete_purging_session(session_id) {
            warn!(%e, session_id, "删除 purging 记录失败，保留记录供重试");
            emit_error(&app_handle, index, total, session_id);
            continue;
        }

        succeeded.push(session_id.clone());

        let _ = app_handle.emit(
            "purge_progress",
            PurgeProgress {
                current: index + 1,
                total,
                session_id: session_id.clone(),
                title: String::new(),
                status: "done".to_string(),
            },
        );
    }

    Ok(succeeded.len())
}

/// 清理单个会话的磁盘资源（media/teams/trash 文件）。不含 PTY/browser 运行时。
fn purge_session_resources(
    storage_root: &std::path::Path,
    media_root: &std::path::Path,
    session_id: &str,
) -> Result<(), String> {
    // 删媒体目录（按会话隔离的新版结构）。
    let session_media = media_root.join(session_id);
    if session_media.exists() {
        std::fs::remove_dir_all(&session_media)
            .map_err(|error| format!("删除媒体目录失败：{error}"))?;
    }
    // 删 teams 目录。
    let teams_dir = storage_root.join("teams").join(session_id);
    if teams_dir.exists() {
        std::fs::remove_dir_all(&teams_dir)
            .map_err(|error| format!("删除 teams 目录失败：{error}"))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_input_cache(
    cache_key: String,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let cache_lock = state.input_cache_update_lock(&cache_key);
    let _cache_guard = cache_lock.lock_owned().await;
    let send_lock = state.session_send_lock(&cache_key);
    let _send_guard = send_lock.lock_owned().await;
    let mut attachments = state
        .with_state_read(|core_state| {
            Ok(crate::state_ops::input_cache(core_state, &cache_key).attachments)
        })
        .await?;
    state
        .with_state(|core_state| {
            core_state.input_caches.remove(&cache_key);
            Ok(())
        })
        .await?;
    attachments.extend(raw_attachments_for_paths(
        state.release_any_input_send_claim(&cache_key),
    ));
    state.mark_input_cache_discarded(&cache_key);
    cleanup_unreferenced_input_attachments(state.inner(), attachments).await;
    Ok(())
}

/// 获取 Desktop 工作空间目录
#[tauri::command]
pub async fn get_workspace_dir(state: State<'_, TiangongApp>) -> Result<String, String> {
    state
        .with_state_read(|core_state| Ok(core_state.workspace_dir.as_str().to_string()))
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
    let mut config = state
        .with_state_read(|core_state| Ok(core_state.config.clone()))
        .await?;
    config.workspace_dir = workspace_dir.clone();
    tiangong_config::registry::update(config.clone()).map_err(|error| error.to_string())?;
    state
        .with_state(|core_state| {
            core_state.config = config;
            crate::session_ops::update_workspace_dir(
                core_state,
                &state.core_manager,
                workspace_dir.clone(),
            )
        })
        .await?;

    // 同步终端：更新终端默认 cwd（后续懒创建的 PTY），并对所有存活 PTY
    // 发送 cd 使已打开的终端进入新 workspace。
    tiangong_plugin_terminal::sync_workspace_cwd(&app, &workspace_dir);

    // cwd 由 app-state 快照维护，下次 turn 从快照重载，无需投递到 worker。

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
    crate::session_ops::update_session_cwd(&state.core_manager, &session_id, cwd)
        .map_err(|error| error.to_string())?;

    // cwd 由 app-state 快照维护，下次 turn 从快照重载，无需投递到 worker。

    Ok(())
}

// ============================================================================
// MCP 管理
// ============================================================================

/// 经运行时 sidecar 通道调用 MCP 插件操作（Desktop 入口）。
fn mcp_invoke(operation: &str, payload: serde_json::Value) -> Result<serde_json::Value, String> {
    let storage_root = tiangong_config::io::storage_root();
    tiangong_plugin_runtime::registry::invoke_sidecar(&storage_root, "mcp", operation, payload)
        .map_err(|e| e.to_string())
}

/// 获取 MCP 服务器列表
#[tauri::command]
pub async fn get_mcp_servers(_state: State<'_, TiangongApp>) -> Result<Vec<McpServerView>, String> {
    let servers: Vec<tiangong_plugin_mcp_protocol::config::McpServerConfig> =
        serde_json::from_value::<tiangong_plugin_mcp_protocol::management::ServersResponse>(
            mcp_invoke("mcp.server.list", serde_json::json!({}))?,
        )
        .map_err(|e: serde_json::Error| e.to_string())?
        .servers;
    Ok(servers.iter().map(McpServerView::from_core).collect())
}

/// 获取 MCP 服务器健康状态
#[tauri::command]
pub async fn get_mcp_health(
    _state: State<'_, TiangongApp>,
) -> Result<Vec<serde_json::Value>, String> {
    let response: tiangong_plugin_mcp_protocol::query::HealthResponse =
        serde_json::from_value(mcp_invoke("mcp.server.health", serde_json::json!({}))?)
            .map_err(|e: serde_json::Error| e.to_string())?;
    response
        .statuses
        .into_iter()
        .map(|s| serde_json::to_value(s).map_err(|e| e.to_string()))
        .collect()
}

/// 探测单个 MCP 服务器（按 name），写回健康缓存。供前端添加/编辑/重试后刷新该行。
#[tauri::command]
pub async fn probe_mcp_server(name: String, _state: State<'_, TiangongApp>) -> Result<(), String> {
    mcp_invoke(
        "mcp.server.probe",
        serde_json::to_value(tiangong_plugin_mcp_protocol::query::ServerNameRequest { name })
            .map_err(|e| e.to_string())?,
    )?;
    Ok(())
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
    use tiangong_plugin_mcp_protocol::config::{
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
    let response: tiangong_plugin_mcp_protocol::MessageResponse =
        serde_json::from_value(mcp_invoke(
            "mcp.server.register",
            serde_json::to_value(&request).map_err(|e| e.to_string())?,
        )?)
        .map_err(|e: serde_json::Error| e.to_string())?;
    state.sync_core_config_from_state().await?;
    Ok(response.message)
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
    use tiangong_plugin_mcp_protocol::config::{
        McpTransportMode, RegisterMcpServerOptions, RegisterMcpServerRequest,
    };
    use tiangong_plugin_mcp_protocol::management::UpdateServerRequest;

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
    let request = UpdateServerRequest {
        name: name.clone(),
        request: RegisterMcpServerRequest {
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
            },
        },
    };
    let response: tiangong_plugin_mcp_protocol::MessageResponse =
        serde_json::from_value(mcp_invoke(
            "mcp.server.update",
            serde_json::to_value(&request).map_err(|e| e.to_string())?,
        )?)
        .map_err(|e: serde_json::Error| e.to_string())?;
    state.sync_core_config_from_state().await?;
    Ok(response.message)
}

/// 移除 MCP 服务器
#[tauri::command]
pub async fn remove_mcp_server(
    name: String,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    use tiangong_plugin_mcp_protocol::management::RemoveServerRequest;
    let response: tiangong_plugin_mcp_protocol::MessageResponse =
        serde_json::from_value(mcp_invoke(
            "mcp.server.remove",
            serde_json::to_value(RemoveServerRequest { name }).map_err(|e| e.to_string())?,
        )?)
        .map_err(|e: serde_json::Error| e.to_string())?;
    state.sync_core_config_from_state().await?;
    Ok(response.message)
}

/// 设置 MCP 服务器启用状态
#[tauri::command]
pub async fn set_mcp_server_enabled(
    name: String,
    enabled: bool,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    use tiangong_plugin_mcp_protocol::management::SetEnabledRequest;
    let response: tiangong_plugin_mcp_protocol::MessageResponse =
        serde_json::from_value(mcp_invoke(
            "mcp.server.set_enabled",
            serde_json::to_value(SetEnabledRequest { name, enabled }).map_err(|e| e.to_string())?,
        )?)
        .map_err(|e: serde_json::Error| e.to_string())?;
    state.sync_core_config_from_state().await?;
    Ok(response.message)
}

// ============================================================================
// 移动端控制（bot）管理
// ============================================================================

fn validate_bot_id(id: String) -> Result<tiangong_bots::BotId, String> {
    tiangong_bots::BotId::try_from(id).map_err(|error| error.to_string())
}

/// 获取已注册的 bot 列表
#[tauri::command]
pub async fn bot_list(
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_bots::BotConfig>, String> {
    Ok(state.bot_store.list())
}

/// 获取指定 bot 的健康状态
#[tauri::command]
pub async fn bot_health(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_bots::BotHealth, String> {
    let id = validate_bot_id(id)?;
    Ok(state.bot_runtime.health(&id).await)
}

/// 获取指定 bot 当前日志文件的最近内容。
#[tauri::command]
pub async fn bot_log(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_bots::BotLog, String> {
    let id = validate_bot_id(id)?;
    if state.bot_store.get(&id).is_none() {
        return Err(format!("bot 不存在：{id}"));
    }
    tiangong_bots::read_log_tail(&id).map_err(|err| format!("读取 bot 日志失败：{err}"))
}

/// 获取 bot 的配置字段 schema（供前端动态渲染表单）。
///
/// schema 权威来源是 bot 二进制 `--describe` 上报（缓存在 ~/.tiangong/bots/<id>/schema.json）。
/// 优先读缓存；若该 bot 实例尚未安装制品，从 bots-index.json 的预览 schema 读取。
#[tauri::command]
pub async fn bot_config_schema(
    bot_id: Option<String>,
    artifact_id: String,
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_bots::ConfigFieldSchema>, String> {
    let bot_id = bot_id.map(validate_bot_id).transpose()?;
    let local_artifacts = state.bot_runtime.scan_local_artifacts();

    // 1) 本地自有 bot 没有版本记录，每次打开配置时重新读取 --describe。
    if let Some(id) = &bot_id {
        if local_artifacts
            .iter()
            .any(|local| local.id == id.as_str() && local.version.is_empty())
        {
            return tiangong_bots::describe_and_cache(id)
                .await
                .map_err(|err| format!("读取本地 Bot 配置失败：{err}"));
        }
        // 线上安装的 bot 优先使用安装时验证并缓存的 schema。
        if let Some(schema) = tiangong_bots::cached_schema(id) {
            return Ok(schema);
        }
    }
    // 2) 扫描本地制品；没有缓存时直接调用本地 bot --describe。
    for local in local_artifacts {
        if local.artifact_id == artifact_id {
            let local_id = validate_bot_id(local.id)?;
            if !local.version.is_empty() {
                if let Some(schema) = tiangong_bots::cached_schema(&local_id) {
                    return Ok(schema);
                }
            }
            return tiangong_bots::describe_and_cache(&local_id)
                .await
                .map_err(|err| format!("读取本地 Bot 配置失败：{err}"));
        }
    }
    // 3) 回退到 bots-index.json 的预览 schema（安装前展示）。
    match state.bot_runtime.fetch_index().await {
        Ok(index) => {
            let manifest = index
                .bots
                .into_iter()
                .find(|m| m.id == artifact_id)
                .ok_or_else(|| format!("未找到制品：{artifact_id}"))?;
            Ok(manifest.config_schema)
        }
        Err(e) => Err(format!("获取 schema 失败（线上不可达且本地无匹配）：{e}")),
    }
}

/// 为支持扫码配置的 bot 创建授权会话。
#[tauri::command]
pub async fn bot_provision_begin(
    bot_id: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_bots::QrSession, String> {
    let bot_id = validate_bot_id(bot_id)?;
    state
        .bot_runtime
        .provision_begin(&bot_id)
        .await
        .map_err(|err| err.to_string())
}

/// 轮询 bot 扫码授权状态。
#[tauri::command]
pub async fn bot_provision_poll(
    bot_id: String,
    session: tiangong_bots::QrSession,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_bots::ProvisionStatus, String> {
    let bot_id = validate_bot_id(bot_id)?;
    state
        .bot_runtime
        .provision_poll(&bot_id, &session)
        .await
        .map_err(|err| err.to_string())
}

/// 拉取远端 bots-index.json（可安装的 bot 列表）。
///
/// 线上不可达时返回明确错误；前端仍会独立加载并展示本地制品。
#[tauri::command]
pub async fn bot_available(
    state: State<'_, TiangongApp>,
) -> Result<tiangong_bots::BotsIndex, String> {
    state
        .bot_runtime
        .fetch_index()
        .await
        .map_err(|err| format!("加载线上 Bot 目录失败：{err}"))
}

/// 扫描本地已安装的制品（`~/.tiangong/bots/*/`）。
///
/// 不依赖线上 bots-index——本地已放置的 bot 二进制 + schema.json 即可被发现。
#[tauri::command]
pub async fn bot_scan_local(
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_bots::LocalArtifact>, String> {
    Ok(state.bot_runtime.scan_local_artifacts())
}

/// 获取 Bot 已发现的主动推送目标。
#[tauri::command]
pub async fn bot_push_targets(
    id: String,
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_bots::PushTargetView>, String> {
    let id = validate_bot_id(id)?;
    if state.bot_store.get(&id).is_none() {
        return Err(format!("bot 不存在：{id}"));
    }
    state
        .bot_runtime
        .push_targets(&id)
        .await
        .map_err(|error| error.to_string())
}

/// 删除一个 Bot 主动推送授权目标。
#[tauri::command]
pub async fn bot_delete_push_target(
    id: String,
    target_id: String,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let id = validate_bot_id(id)?;
    if state.bot_store.get(&id).is_none() {
        return Err(format!("bot 不存在：{id}"));
    }
    state
        .bot_runtime
        .delete_push_target(&id, &target_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok("推送授权已删除".to_string())
}

fn bot_mcp_connection_matches(
    existing: &tiangong_plugin_mcp_protocol::config::McpServerConfig,
    generated: &tiangong_bots::BotMcpConfig,
) -> bool {
    use tiangong_plugin_mcp_protocol::config::ResolvedMcpTransport;

    existing.resolved_transport() == ResolvedMcpTransport::Stdio
        && existing.command == generated.command
        && existing.args == generated.args
        && existing.endpoint.is_empty()
        && existing.auth_header.is_empty()
        && existing.headers.is_empty()
        && existing.env.is_empty()
        && existing.tags == generated.tags
}

fn bot_mcp_registration_request(
    generated: &tiangong_bots::BotMcpConfig,
    enabled: bool,
) -> tiangong_plugin_mcp_protocol::config::RegisterMcpServerRequest {
    use tiangong_plugin_mcp_protocol::config::{
        McpTransportMode, RegisterMcpServerOptions, RegisterMcpServerRequest,
    };

    RegisterMcpServerRequest {
        name: generated.name.clone(),
        command: generated.command.clone(),
        args: generated.args.clone(),
        tags: generated.tags.clone(),
        enabled,
        options: RegisterMcpServerOptions {
            transport: Some(McpTransportMode::Stdio),
            ..Default::default()
        },
    }
}

/// 查询当前 MCP server 列表（经 sidecar 通道）。
fn list_mcp_servers_via_sidecar(
    _state: &TiangongApp,
) -> Result<Vec<tiangong_plugin_mcp_protocol::config::McpServerConfig>, String> {
    serde_json::from_value::<tiangong_plugin_mcp_protocol::management::ServersResponse>(mcp_invoke(
        "mcp.server.list",
        serde_json::json!({}),
    )?)
    .map_err(|e: serde_json::Error| e.to_string())
    .map(|r| r.servers)
}

/// 经运行时 sidecar 通道调用 MCP 插件操作（&TiangongApp 重载，复用 mcp_invoke）。
fn mcp_invoke_state(
    _state: &TiangongApp,
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    mcp_invoke(operation, payload)
}

/// 为支持 MCP 的 Bot 确保对应普通 MCP 已注册并启用。
pub async fn ensure_bot_mcp_registered(
    id: &tiangong_bots::BotId,
    state: &TiangongApp,
) -> Result<Option<String>, String> {
    use tiangong_plugin_mcp_protocol::management::{
        SetEnabledRequest, SERVER_SET_ENABLED_OPERATION,
    };
    let supports_mcp = state
        .bot_runtime
        .supports_mcp(id)
        .await
        .map_err(|error| format!("读取 Bot MCP 能力失败：{error}"))?;
    if !supports_mcp {
        return Ok(None);
    }

    let generated = state
        .bot_runtime
        .generate_mcp_config(id)
        .await
        .map_err(|error| format!("生成 Bot MCP 配置失败：{error}"))?;
    let name = generated.name.clone();

    if let Some(existing) = list_mcp_servers_via_sidecar(state)?
        .into_iter()
        .find(|server| server.name == name)
    {
        if !bot_mcp_connection_matches(&existing, &generated) {
            return Err(format!("MCP 名称 {name} 已被其他配置占用，未自动覆盖"));
        }
        if !existing.enabled {
            let response: tiangong_plugin_mcp_protocol::MessageResponse =
                serde_json::from_value(mcp_invoke_state(
                    state,
                    SERVER_SET_ENABLED_OPERATION,
                    serde_json::to_value(SetEnabledRequest {
                        name: name.clone(),
                        enabled: true,
                    })
                    .map_err(|e| e.to_string())?,
                )?)
                .map_err(|e: serde_json::Error| e.to_string())?;
            if let Err(sync_error) = state.sync_core_config_from_state().await {
                let rollback = mcp_invoke_state(
                    state,
                    SERVER_SET_ENABLED_OPERATION,
                    serde_json::to_value(SetEnabledRequest {
                        name: name.clone(),
                        enabled: false,
                    })
                    .unwrap_or_default(),
                )
                .map(|_| "已恢复为停用".to_string())
                .unwrap_or_else(|error| format!("恢复停用失败：{error}"));
                return Err(format!("同步 MCP 配置失败：{sync_error}；{rollback}"));
            }
            return Ok(Some(response.message));
        }
        return Ok(Some(format!("MCP 已注册：{name}")));
    }

    let request = bot_mcp_registration_request(&generated, generated.enabled);
    let response: tiangong_plugin_mcp_protocol::MessageResponse =
        serde_json::from_value(mcp_invoke_state(
            state,
            "mcp.server.register",
            serde_json::to_value(&request).map_err(|e| e.to_string())?,
        )?)
        .map_err(|e: serde_json::Error| e.to_string())?;
    if let Err(sync_error) = state.sync_core_config_from_state().await {
        use tiangong_plugin_mcp_protocol::management::RemoveServerRequest;
        let rollback = mcp_invoke_state(
            state,
            "mcp.server.remove",
            serde_json::to_value(RemoveServerRequest { name: name.clone() }).unwrap_or_default(),
        )
        .map(|_| "已撤销 MCP 注册".to_string())
        .unwrap_or_else(|error| format!("撤销 MCP 注册失败：{error}"));
        return Err(format!("同步 MCP 配置失败：{sync_error}；{rollback}"));
    }
    Ok(Some(response.message))
}

async fn unregister_bot_mcp(
    id: &tiangong_bots::BotId,
    state: &TiangongApp,
) -> Result<bool, String> {
    use tiangong_plugin_mcp_protocol::management::RemoveServerRequest;
    let supports_mcp = state
        .bot_runtime
        .supports_mcp(id)
        .await
        .map_err(|error| format!("读取 Bot MCP 能力失败：{error}"))?;
    if !supports_mcp {
        return Ok(false);
    }

    let generated = state
        .bot_runtime
        .generate_mcp_config(id)
        .await
        .map_err(|error| format!("生成 Bot MCP 配置失败：{error}"))?;
    let Some(existing) = list_mcp_servers_via_sidecar(state)?
        .into_iter()
        .find(|server| server.name == generated.name)
    else {
        return Ok(false);
    };
    if !bot_mcp_connection_matches(&existing, &generated) {
        return Ok(false);
    }

    let _: tiangong_plugin_mcp_protocol::MessageResponse =
        serde_json::from_value(mcp_invoke_state(
            state,
            "mcp.server.remove",
            serde_json::to_value(RemoveServerRequest {
                name: generated.name.clone(),
            })
            .map_err(|e| e.to_string())?,
        )?)
        .map_err(|e: serde_json::Error| e.to_string())?;
    if let Err(sync_error) = state.sync_core_config_from_state().await {
        let request = bot_mcp_registration_request(&generated, existing.enabled);
        let rollback = mcp_invoke_state(
            state,
            "mcp.server.register",
            serde_json::to_value(&request).unwrap_or_default(),
        )
        .map(|_| "已恢复 MCP 注册".to_string())
        .unwrap_or_else(|error| format!("恢复 MCP 注册失败：{error}"));
        return Err(format!("同步 MCP 配置失败：{sync_error}；{rollback}"));
    }
    Ok(true)
}

/// 把 Bot 声明的出站能力注册为普通 stdio MCP。
#[tauri::command]
pub async fn bot_register_mcp(id: String, state: State<'_, TiangongApp>) -> Result<String, String> {
    let id = validate_bot_id(id)?;
    if state.bot_store.get(&id).is_none() {
        return Err(format!("bot 不存在：{id}"));
    }
    ensure_bot_mcp_registered(&id, state.inner())
        .await?
        .ok_or_else(|| "该 Bot 不支持 MCP".to_string())
}

/// 注册新 bot（不启动；需先 `bot_install` 下载制品再 `bot_start`）
#[tauri::command]
pub async fn bot_register(
    request: tiangong_bots::RegisterBotRequest,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_bots::BotConfig, String> {
    tiangong_bots::BotId::try_from(request.id.as_str()).map_err(|error| error.to_string())?;
    state
        .bot_store
        .register(request)
        .map_err(|error| error.to_string())
}

/// 更新已有 bot（id 主键不变）
#[tauri::command]
pub async fn bot_update(
    id: String,
    request: tiangong_bots::UpdateBotRequest,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_bots::BotConfig, String> {
    let id = validate_bot_id(id)?;
    state
        .bot_store
        .update(&id, request)
        .map_err(|error| error.to_string())
}

/// 删除 bot 配置（若运行中则先停止），保留已安装制品和运行目录。
#[tauri::command]
pub async fn bot_remove(id: String, state: State<'_, TiangongApp>) -> Result<String, String> {
    let id = validate_bot_id(id)?;
    if state.bot_store.get(&id).is_none() {
        return Err(format!("bot 不存在：{id}"));
    }
    let mcp_removed = unregister_bot_mcp(&id, state.inner()).await?;
    if let Err(stop_error) = state.bot_runtime.stop(&id).await {
        let recovery = if mcp_removed {
            ensure_bot_mcp_registered(&id, state.inner())
                .await
                .map(|_| "已恢复 MCP 注册".to_string())
                .unwrap_or_else(|error| format!("恢复 MCP 注册失败：{error}"))
        } else {
            "MCP 状态无需恢复".to_string()
        };
        return Err(format!(
            "停止 bot 失败，未删除配置：{stop_error}；{recovery}"
        ));
    }
    if let Err(remove_error) = state.bot_store.remove(&id) {
        let recovery = if mcp_removed {
            ensure_bot_mcp_registered(&id, state.inner())
                .await
                .map(|_| "已恢复 MCP 注册".to_string())
                .unwrap_or_else(|error| format!("恢复 MCP 注册失败：{error}"))
        } else {
            "MCP 状态无需恢复".to_string()
        };
        return Err(format!("删除 bot 配置失败：{remove_error}；{recovery}"));
    }
    Ok("bot 配置已删除，已安装程序保留".to_string())
}

/// 下载某制品到指定 bot 实例目录
#[tauri::command]
pub async fn bot_install(
    artifact_id: String,
    dest_bot_id: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let dest_bot_id = validate_bot_id(dest_bot_id)?;
    let index = state
        .bot_runtime
        .fetch_index()
        .await
        .map_err(|e| e.to_string())?;
    let manifest = index
        .bots
        .into_iter()
        .find(|m| m.id == artifact_id)
        .ok_or_else(|| format!("bots-index 中未找到制品：{artifact_id}"))?;
    let progress: tiangong_bots::ProgressFn = std::sync::Arc::new({
        let app = app.clone();
        let total = manifest.current_artifact().map(|_| 0u64).unwrap_or(0);
        let _ = total;
        move |downloaded, content_len| {
            let _ = app.emit(
                "bot_install_progress",
                serde_json::json!({ "downloaded": downloaded, "total": content_len }),
            );
        }
    });
    state
        .bot_runtime
        .install(manifest, &dest_bot_id, Some(progress))
        .await
        .map_err(|e| e.to_string())?;
    Ok("制品安装完成".to_string())
}

/// 启动指定 bot 实例（需制品已安装）。
///
/// 启动 bot 前自动确保 embedded server 已运行（未运行则启动），
/// bot 需要通过 server 的 /api/v1/messages 收发消息。
#[tauri::command]
pub async fn bot_start(id: String, state: State<'_, TiangongApp>) -> Result<String, String> {
    let id = validate_bot_id(id)?;
    let bot = state
        .bot_store
        .get(&id)
        .ok_or_else(|| format!("bot 不存在：{id}"))?;

    // 读取 Server 配置生成 bot 回连 env，但不强制启动 Server（方案：bot 独立运行）。
    let server_config = state
        .with_state_read(|core_state| Ok(core_state.config.server.clone()))
        .await
        .map_err(|e| e.to_string())?;
    let extra_env = bot_server_env(&server_config);

    state
        .bot_runtime
        .start(&bot, &extra_env)
        .await
        .map_err(|e| e.to_string())?;
    save_started_bot_state(state.bot_store.as_ref(), &id, || async {
        state
            .bot_runtime
            .stop(&id)
            .await
            .map_err(|error| error.to_string())
    })
    .await?;
    // 提示 Server 状态（不阻止 bot 启动）。
    if !server_health_check(&server_config) {
        return Ok(format!(
            "bot 已启动：{}\n当前未检测到天工 Server，Bot 暂时无法调用 Agent。             请启动 Server 后 Bot 将自动恢复连接。",
            bot.id
        ));
    }
    Ok(format!("bot 已启动：{}", bot.id))
}

/// 停止指定 bot 实例
#[tauri::command]
pub async fn bot_stop(id: String, state: State<'_, TiangongApp>) -> Result<String, String> {
    let id = validate_bot_id(id)?;
    let mcp_removed = unregister_bot_mcp(&id, state.inner()).await?;
    let stop_result = stop_bot_with_state(state.bot_store.as_ref(), &id, || async {
        state
            .bot_runtime
            .stop(&id)
            .await
            .map_err(|error| error.to_string())
    })
    .await;
    if let Err(stop_error) = stop_result {
        let recovery = if mcp_removed {
            ensure_bot_mcp_registered(&id, state.inner())
                .await
                .map(|_| "已恢复 MCP 注册".to_string())
                .unwrap_or_else(|error| format!("恢复 MCP 注册失败：{error}"))
        } else {
            "MCP 状态无需恢复".to_string()
        };
        return Err(format!("{stop_error}；{recovery}"));
    }
    Ok("bot 已停止".to_string())
}

async fn save_started_bot_state<F, Fut>(
    store: &tiangong_bots::BotStore,
    id: &tiangong_bots::BotId,
    rollback_stop: F,
) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    if let Err(save_error) = store.set_enabled(id, true) {
        return match rollback_stop().await {
            Ok(()) => Err(format!(
                "保存自动运行状态失败，已撤销本次启动：{save_error}"
            )),
            Err(stop_error) => Err(format!(
                "保存自动运行状态失败，且 bot 未能停止；请立即检查运行状态：保存错误={save_error}，停止错误={stop_error}"
            )),
        };
    }
    Ok(())
}

async fn stop_bot_with_state<F, Fut>(
    store: &tiangong_bots::BotStore,
    id: &tiangong_bots::BotId,
    stop: F,
) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let original = store.get(id).ok_or_else(|| format!("bot 不存在：{id}"))?;
    store
        .set_enabled(id, false)
        .map_err(|error| format!("取消自动运行失败，bot 未停止：{error}"))?;

    if let Err(stop_error) = stop().await {
        if !original.enabled {
            return Err(format!("停止 bot 失败，自动运行状态仍为关闭：{stop_error}"));
        }
        return match store.set_enabled(id, true) {
            Ok(_) => Err(format!(
                "停止 bot 失败，已恢复自动运行状态：{stop_error}"
            )),
            Err(restore_error) => Err(format!(
                "停止 bot 失败，且自动运行状态恢复失败；请立即检查运行状态：停止错误={stop_error}，恢复错误={restore_error}"
            )),
        };
    }
    Ok(())
}

/// 检查某制品是否有线上更新。
///
/// 返回 `Some(manifest)` 表示有更新（含版本号供前端展示），`None` 表示已是最新。
#[tauri::command]
pub async fn bot_check_update(
    artifact_id: String,
    state: State<'_, TiangongApp>,
) -> Result<Option<tiangong_bots::BotManifest>, String> {
    state
        .bot_runtime
        .check_update(&artifact_id)
        .await
        .map_err(|e| e.to_string())
}

/// 升级 bot 制品（记录原状态 → 停止 → 下载新版本 → 恢复原状态）。
#[tauri::command]
pub async fn bot_upgrade(
    bot_id: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let bot_id = validate_bot_id(bot_id)?;
    // 先查 bot 配置拿 artifact_id。
    let bot = state
        .bot_store
        .get(&bot_id)
        .ok_or_else(|| format!("bot 不存在：{bot_id}"))?;
    let artifact_id = bot.artifact_id.clone();
    let was_running = state.bot_runtime.is_running(&bot_id).await;
    let restart_env = if was_running {
        let server_config = state
            .with_state_read(|core_state| Ok(core_state.config.server.clone()))
            .await?;
        Some(bot_server_env(&server_config))
    } else {
        None
    };

    // 拉取线上 manifest。
    let manifest = state
        .bot_runtime
        .check_update(&artifact_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "已是最新版本".to_string())?;

    let progress: tiangong_bots::ProgressFn = std::sync::Arc::new({
        let app = app.clone();
        move |downloaded, content_len| {
            let _ = app.emit(
                "bot_upgrade_progress",
                serde_json::json!({ "downloaded": downloaded, "total": content_len }),
            );
        }
    });
    let mcp_removed = unregister_bot_mcp(&bot_id, state.inner()).await?;
    if let Err(error) = state.bot_store.set_enabled(&bot_id, false) {
        let recovery = if mcp_removed {
            ensure_bot_mcp_registered(&bot_id, state.inner())
                .await
                .map(|_| "已恢复 MCP 注册".to_string())
                .unwrap_or_else(|restore_error| format!("恢复 MCP 注册失败：{restore_error}"))
        } else {
            "MCP 状态无需恢复".to_string()
        };
        return Err(format!(
            "升级前取消自动运行失败，未开始升级：{error}；{recovery}"
        ));
    }

    let upgrade_result = state
        .bot_runtime
        .upgrade(&bot_id, manifest, Some(progress))
        .await;
    if let Err(upgrade_error) = upgrade_result {
        let mut recovery = Vec::new();
        let is_running = state.bot_runtime.is_running(&bot_id).await;
        if was_running && !is_running {
            match state
                .bot_runtime
                .start(
                    &bot,
                    restart_env
                        .as_ref()
                        .expect("运行中的 bot 必须准备恢复环境变量"),
                )
                .await
            {
                Ok(()) => recovery.push("原运行状态已恢复".to_string()),
                Err(error) => recovery.push(format!("恢复运行失败：{error}")),
            }
        } else if !was_running && is_running {
            match state.bot_runtime.stop(&bot_id).await {
                Ok(()) => recovery.push("原停止状态已恢复".to_string()),
                Err(error) => recovery.push(format!("恢复停止状态失败：{error}")),
            }
        } else {
            recovery.push("运行状态无需恢复".to_string());
        }

        if bot.enabled {
            match state.bot_store.set_enabled(&bot_id, true) {
                Ok(_) => recovery.push("自动运行状态已恢复".to_string()),
                Err(error) => recovery.push(format!("恢复自动运行状态失败：{error}")),
            }
        } else {
            recovery.push("自动运行状态仍为关闭".to_string());
        }

        if state.bot_runtime.is_running(&bot_id).await {
            match ensure_bot_mcp_registered(&bot_id, state.inner()).await {
                Ok(Some(_)) => recovery.push("MCP 注册已恢复".to_string()),
                Ok(None) => recovery.push("Bot 不需要 MCP 注册".to_string()),
                Err(error) => recovery.push(format!("恢复 MCP 注册失败：{error}")),
            }
        } else if mcp_removed {
            recovery.push("Bot 未恢复运行，MCP 保持注销".to_string());
        }

        return Err(format!(
            "升级失败：{upgrade_error}；恢复结果：{}",
            recovery.join("；")
        ));
    }

    let mut restore_errors = Vec::new();
    let mut restored_running = !was_running;
    if was_running {
        match state
            .bot_runtime
            .start(
                &bot,
                restart_env
                    .as_ref()
                    .expect("运行中的 bot 必须准备恢复环境变量"),
            )
            .await
        {
            Ok(()) => restored_running = true,
            Err(error) => restore_errors.push(format!("恢复运行失败：{error}")),
        }
    }

    if bot.enabled {
        if let Err(error) = state.bot_store.set_enabled(&bot_id, true) {
            restore_errors.push(format!("恢复自动运行状态失败：{error}"));
        }
    }

    if was_running && restored_running {
        match ensure_bot_mcp_registered(&bot_id, state.inner()).await {
            Ok(_) => {}
            Err(error) => restore_errors.push(format!("恢复 MCP 注册失败：{error}")),
        }
    }

    if !restore_errors.is_empty() {
        return Err(format!(
            "升级已完成，但恢复原状态失败：{}",
            restore_errors.join("；")
        ));
    }

    if was_running {
        Ok("升级完成，Bot 已恢复运行".to_string())
    } else {
        Ok("升级完成，Bot 保持停止".to_string())
    }
}

// ============================================================================
// Skill 管理
//
// skill 列表/详情/启停/删除/env 编辑等管理操作全部经插件设置页（WASM UI）
// 走 handle_view_message 通道，不再暴露 Tauri 命令。@提及补全候选由 skill
// WASM 插件的 mention-candidates 导出贡献，经 Core.get_mentions 聚合。
// ============================================================================

// ============================================================================
// Server 管理
// ============================================================================

/// 获取 Server 配置
#[tauri::command]
pub async fn get_server_config(state: State<'_, TiangongApp>) -> Result<ServerConfigView, String> {
    let config = state
        .with_state_read(|core_state| Ok(core_state.config.server.clone()))
        .await?;
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
    state: State<'_, TiangongApp>,
) -> Result<String, String> {
    let current = state
        .with_state_read(|core_state| Ok(core_state.config.server.clone()))
        .await?;
    let config = tiangong_server::config::ServerConfig {
        host,
        port,
        auth_token: auth_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
            .or(current.auth_token),
        enabled: current.enabled,
    };
    save_server_config_to_state(state.inner(), config).await?;
    Ok("Server 配置已保存".to_string())
}

/// 启动嵌入式 Server（Desktop 模式下 Server 运行在 app 进程内）
#[tauri::command]
pub async fn start_server(state: State<'_, TiangongApp>) -> Result<String, String> {
    let config = state
        .with_state_read(|core_state| Ok(core_state.config.server.clone()))
        .await?;

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
    if let Err(err) = wait_for_server_health(&config).await {
        let _ = state.stop_embedded_server();
        return Err(err);
    }

    // 持久化 enabled 标记，重启后自动拉起
    let mut config = config;
    config.enabled = true;
    if let Err(error) = save_server_config_to_state(state.inner(), config.clone()).await {
        let _ = state.stop_embedded_server();
        return Err(error);
    }

    Ok(format!("Server 已启动：{}:{}", config.host, config.port))
}

/// 停止 Server
#[tauri::command]
pub async fn stop_server(state: State<'_, TiangongApp>) -> Result<String, String> {
    let config = state
        .with_state_read(|core_state| Ok(core_state.config.server.clone()))
        .await?;

    // 优先停止嵌入式 server
    if state.is_embedded_server_running() {
        state.stop_embedded_server()?;

        // 持久化 enabled 标记
        let mut config = config;
        config.enabled = false;
        save_server_config_to_state(state.inner(), config).await?;

        return Ok("Server 已停止".to_string());
    }

    // 兜底：检查是否有外部 server 进程
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
    save_server_config_to_state(state.inner(), config).await?;

    Ok("Server 已停止".to_string())
}

async fn save_server_config_to_state(
    state: &TiangongApp,
    config: tiangong_server::config::ServerConfig,
) -> Result<(), String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    tiangong_config::save_server_config_to_dir(&storage_root, &config)
        .map_err(|error| error.to_string())?;
    state
        .with_state(|core_state| {
            core_state.config.server = config;
            Ok(())
        })
        .await
}

#[allow(dead_code)]
pub async fn ensure_server_running_for_bots(
    state: &TiangongApp,
) -> Result<tiangong_server::config::ServerConfig, String> {
    let mut config = state
        .with_state_read(|core_state| Ok(core_state.config.server.clone()))
        .await?;
    if server_health_check(&config) {
        return Ok(config);
    }

    let started_here = if state.is_embedded_server_running() {
        false
    } else {
        tracing::info!("bot 启动前自动启用 embedded server");
        state.start_embedded_server(&config.host, config.port, config.auth_token.clone())?;
        true
    };

    if let Err(error) = wait_for_server_health(&config).await {
        if started_here {
            let _ = state.stop_embedded_server();
        }
        return Err(format!(
            "启动 embedded server 失败（bot 无法收发消息）：{error}"
        ));
    }

    if !config.enabled {
        config.enabled = true;
        if let Err(error) = save_server_config_to_state(state, config.clone()).await {
            if started_here {
                let _ = state.stop_embedded_server();
            }
            return Err(format!("保存 Server 自动运行状态失败：{error}"));
        }
    }
    Ok(config)
}

pub fn bot_server_env(
    config: &tiangong_server::config::ServerConfig,
) -> std::collections::BTreeMap<String, String> {
    tiangong_bots::server_env(&config.host, config.port, config.auth_token.clone())
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

async fn wait_for_server_health(
    config: &tiangong_server::config::ServerConfig,
) -> Result<(), String> {
    for _ in 0..30 {
        if server_health_check(config) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
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

pub fn server_health_check(config: &tiangong_server::config::ServerConfig) -> bool {
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

pub fn connect_host(host: &str) -> String {
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
        .with_state_read(|core_state| Ok(ModelsConfigView::from_core(&core_state.config.models)))
        .await
}

/// 设置模型配置
#[tauri::command]
pub async fn set_models_config(
    config: ModelsConfigView,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let current = state
        .with_state_read(|core_state| Ok(core_state.config.clone()))
        .await?;
    let config = tiangong_config::registry::update_models(&current, config.to_core())
        .map_err(|error| error.to_string())?;
    state
        .with_state(|core_state| {
            core_state.config = config;
            Ok(())
        })
        .await?;
    state.sync_core_config_from_state().await?;
    Ok(())
}

// ── 索引预热 ──

/// 预热指定路径的工作区索引。
///
/// 后台静默调用（创建新对话/切换工作目录时触发，无用户交互），经 sidecar 通道
/// 转发：索引已存在则直接返回，否则后台扫描立即返回不阻塞。索引管理（列表/
/// 删除/重建）由「设置 → 索引管理」页经插件 UI 通道处理，不在此处。
#[tauri::command]
pub async fn prewarm_workspace_index(root: String) -> Result<(), String> {
    let storage_root = tiangong_config::io::storage_root();
    let payload = serde_json::to_value(
        tiangong_plugin_index_protocol::management::PrewarmWorkspaceIndexRequest { root },
    )
    .map_err(|e| e.to_string())?;
    tiangong_plugin_runtime::registry::invoke_sidecar(
        &storage_root,
        "index",
        tiangong_plugin_index_protocol::management::PREWARM_WORKSPACE_INDEX_OPERATION,
        payload,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
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
        .with_state_read(|core_state| Ok(core_state.model_list.to_vec()))
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

// ── Webhook 管理 ─────────────────────────────────────────────

#[tauri::command]
pub async fn webhook_list() -> Result<Vec<serde_json::Value>, String> {
    let store = tiangong_server::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
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
    let store = tiangong_server::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
    let now = chrono::Local::now().naive_local().to_string();
    let webhook = tiangong_server::webhook::model::Webhook {
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
    let store = tiangong_server::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
    let req = tiangong_server::webhook::model::UpdateWebhookRequest {
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
    let store = tiangong_server::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
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
    // 校验 webhook 存在（CRUD 仍直接读写本地 store）
    let store = tiangong_server::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
    let webhook = store
        .get(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Webhook '{id}' 不存在"))?;

    // webhook 触发（投递消息到 Core）是 server 的能力：经嵌入 server 的 HTTP 接口执行，
    // 由 server 用 ServerCoreBackend 投递。需嵌入 server 在线。
    let server_config = state
        .with_state_read(|core_state| Ok(core_state.config.server.clone()))
        .await
        .map_err(|e| e.to_string())?;
    if !server_health_check(&server_config) {
        return Err("Webhook 触发需要先启动内嵌 Server".to_string());
    }
    let host = connect_host(&server_config.host);
    let url = format!(
        "http://{host}:{}/api/v1/webhooks/{id}/trigger",
        server_config.port
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;
    let mut request = client.post(&url);
    if let Some(token) = server_config.auth_token.as_deref() {
        if !token.trim().is_empty() {
            request = request.bearer_auth(token);
        }
    }
    let resp = request
        .send()
        .await
        .map_err(|e| format!("触发 Webhook 失败：{e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("触发 Webhook 失败：HTTP {status}，{body}"));
    }
    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析触发响应失败：{e}"))?;

    Ok(serde_json::json!({
        "webhook_id": webhook.id,
        "session_id": webhook.session_id,
        "status": value.get("status").cloned().unwrap_or(serde_json::json!("triggered")),
    }))
}

#[tauri::command]
pub async fn webhook_list_runs(
    id: String,
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
    let store = tiangong_server::webhook::store::WebhookStore::open().map_err(|e| e.to_string())?;
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
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    use super::{
        cancel_after_session_send_boundary, done_event_keeps_turn_running,
        merge_agent_worker_messages, save_started_bot_state, stop_bot_with_state,
    };
    use tiangong_core::core::Plugin;
    use tiangong_core::tool_override::{
        MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SLOW_FINISH_SAFETY_LIMIT: Duration = Duration::from_secs(20);

    struct SlowTurnFinishedState {
        started: AtomicBool,
        started_notify: tokio::sync::Notify,
        finished: AtomicBool,
        finished_notify: tokio::sync::Notify,
        released: Mutex<bool>,
        released_notify: Condvar,
        safety_limit_reached: AtomicBool,
    }

    impl SlowTurnFinishedState {
        fn new() -> Self {
            Self {
                started: AtomicBool::new(false),
                started_notify: tokio::sync::Notify::new(),
                finished: AtomicBool::new(false),
                finished_notify: tokio::sync::Notify::new(),
                released: Mutex::new(false),
                released_notify: Condvar::new(),
                safety_limit_reached: AtomicBool::new(false),
            }
        }

        fn release(&self) {
            let mut released = self
                .released
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *released = true;
            self.released_notify.notify_all();
        }
    }

    struct SlowTurnFinishedPlugin {
        state: Arc<SlowTurnFinishedState>,
    }

    impl ToolSpecProvider for SlowTurnFinishedPlugin {}
    impl ToolOverrideHandler for SlowTurnFinishedPlugin {}
    impl PromptSectionProvider for SlowTurnFinishedPlugin {}
    impl MentionCandidateProvider for SlowTurnFinishedPlugin {}

    impl Plugin for SlowTurnFinishedPlugin {
        fn id(&self) -> &str {
            "test-slow-turn-finished"
        }

        fn on_turn_started(
            &self,
            _session: &mut tiangong_core::session::Session,
            _turn_start_idx: usize,
        ) {
            self.state.started.store(true, Ordering::Release);
            self.state.started_notify.notify_one();
        }

        fn on_turn_finished(
            &self,
            _session: &mut tiangong_core::session::Session,
            _turn_start_idx: usize,
        ) {
            self.state.finished.store(true, Ordering::Release);
            self.state.finished_notify.notify_one();

            let released = self
                .state
                .released
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let (_released, wait_result) = self
                .state
                .released_notify
                .wait_timeout_while(released, SLOW_FINISH_SAFETY_LIMIT, |released| !*released)
                .unwrap_or_else(|error| error.into_inner());
            if wait_result.timed_out() {
                self.state
                    .safety_limit_reached
                    .store(true, Ordering::Release);
            }
        }
    }

    struct SlowTurnFinishedRelease(Arc<SlowTurnFinishedState>);

    impl Drop for SlowTurnFinishedRelease {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    async fn wait_for_plugin_signal(
        flag: &AtomicBool,
        notify: &tokio::sync::Notify,
        message: &str,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !flag.load(Ordering::Acquire) {
                notify.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{message}"));
    }

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "耗时故障诊断，包含真实 5 秒等待，仅在手动复测时执行"]
    async fn slow_turn_finish_reproduces_append_cleanup_and_same_session_stall() {
        use std::sync::mpsc;

        use tiangong_core::agent_input::AgentInputKind;
        use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
        use tiangong_core::session::Session;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(30))
                    .set_body_raw("data: [DONE]\n\n", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let storage = tempfile::tempdir().expect("应创建隔离存储目录");
        let workspace = storage.path().to_string_lossy().to_string();
        let session_id = format!("running-append-stall-{}", scru128::new());
        let healthy_session_id = format!("healthy-switch-{}", scru128::new());
        let config = CoreConfig::builder()
            .with_chat(&server.uri(), "test-key", "test-model")
            .build();
        let manager = tiangong_core_manager::CoreManager::new(
            CoreConfigProvider::new(config.clone()),
            storage.path(),
        );

        let slow_state = Arc::new(SlowTurnFinishedState::new());
        let _slow_finish_release = SlowTurnFinishedRelease(slow_state.clone());
        let slow_plugin: Arc<dyn Plugin> = Arc::new(SlowTurnFinishedPlugin {
            state: slow_state.clone(),
        });
        let (stream_tx, _stream_rx) = mpsc::channel();
        manager
            .ensure_core(
                &session_id,
                config.clone(),
                workspace.clone(),
                stream_tx,
                move || vec![slow_plugin],
            )
            .await
            .expect("应创建测试 Core");
        assert!(
            manager.deliver_to_core_if_live(&session_id, AgentInputKind::message("first message"))
        );

        wait_for_plugin_signal(
            &slow_state.started,
            &slow_state.started_notify,
            "首轮未在期限内进入插件启动钩子",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if server
                    .received_requests()
                    .await
                    .is_some_and(|requests| !requests.is_empty())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("首轮模型请求未在期限内到达本地假服务");

        let session_send_lock = Arc::new(tokio::sync::Mutex::new(()));
        let append_manager = manager.clone();
        let append_session_id = session_id.clone();
        let append_lock = session_send_lock.clone();
        let (delivery_result_tx, delivery_result_rx) = tokio::sync::oneshot::channel();
        let mut append_task = tokio::spawn(async move {
            let _send_guard = append_lock.lock_owned().await;
            let delivery_started = Instant::now();
            let delivered = append_manager.deliver_to_core_if_live(
                &append_session_id,
                AgentInputKind::message("appended message"),
            );
            let delivery_elapsed = delivery_started.elapsed();
            let _ = delivery_result_tx.send((delivered, delivery_elapsed));
            let cleanup = if delivered {
                Ok(())
            } else {
                append_manager.retire_core(&append_session_id, false).await
            };
            (delivered, cleanup)
        });

        wait_for_plugin_signal(
            &slow_state.finished,
            &slow_state.finished_notify,
            "追加消息取消首轮后未进入同步收尾钩子",
        )
        .await;
        let (delivered, delivery_elapsed) =
            tokio::time::timeout(Duration::from_secs(7), delivery_result_rx)
                .await
                .expect("追加消息未在 Core 的 5 秒上限后返回")
                .expect("追加消息结果通道不应提前关闭");
        assert!(!delivered, "慢收尾期间追加消息应投递失败");
        assert!(
            delivery_elapsed >= Duration::from_secs(5) && delivery_elapsed < Duration::from_secs(7),
            "追加消息应在 5 秒等待上限后失败，实际耗时 {delivery_elapsed:?}"
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            while manager.has_live_core(&session_id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("失败清理开始后 Core 应先从会话表移除");
        assert!(
            !append_task.is_finished(),
            "Core 移除后失败清理仍应等待慢收尾"
        );
        assert!(
            !manager.deliver_to_core_if_live(&session_id, AgentInputKind::message("direct retry")),
            "清理等待期间当前会话已无 Core 可接收消息"
        );

        let retry_lock = session_send_lock.clone();
        let retry_manager = manager.clone();
        let retry_session_id = session_id.clone();
        let (retry_entered_tx, retry_entered_rx) = tokio::sync::oneshot::channel();
        let retry_task = tokio::spawn(async move {
            let _send_guard = retry_lock.lock_owned().await;
            let _ = retry_entered_tx.send(());
            retry_manager.deliver_to_core_if_live(
                &retry_session_id,
                AgentInputKind::message("retry through host boundary"),
            )
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(250), retry_entered_rx)
                .await
                .is_err(),
            "再次发送应等待仍被失败清理持有的会话发送锁"
        );
        retry_task.abort();
        let _ = retry_task.await;

        let cancel_lock = session_send_lock.clone();
        let cancel_manager = manager.clone();
        let cancel_session_id = session_id.clone();
        let (cancel_entered_tx, cancel_entered_rx) = tokio::sync::oneshot::channel();
        let cancel_task = tokio::spawn(async move {
            cancel_after_session_send_boundary(cancel_lock, || {
                let _ = cancel_entered_tx.send(());
                cancel_manager.cancel_core(&cancel_session_id)
            })
            .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(250), cancel_entered_rx)
                .await
                .is_err(),
            "取消命令应等待同一个会话发送锁"
        );
        cancel_task.abort();
        let _ = cancel_task.await;

        let reopen_manager = manager.clone();
        let reopen_session_id = session_id.clone();
        let reopen_config = config.clone();
        let reopen_workspace = workspace.clone();
        let (reopen_stream_tx, _reopen_stream_rx) = mpsc::channel();
        let mut reopen_task = tokio::spawn(async move {
            reopen_manager
                .ensure_core(
                    &reopen_session_id,
                    reopen_config,
                    reopen_workspace,
                    reopen_stream_tx,
                    Vec::new,
                )
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(250), &mut reopen_task)
                .await
                .is_err(),
            "重新打开故障会话应等待失败清理持有的 Core 创建锁"
        );

        let mut healthy_session = Session::new("healthy switch target");
        healthy_session.id = healthy_session_id.clone();
        healthy_session.cwd = workspace.clone();
        healthy_session.bind_storage_root(storage.path());
        healthy_session
            .try_persist_to_disk()
            .expect("应持久化健康切换目标");
        let (healthy_stream_tx, _healthy_stream_rx) = mpsc::channel();
        let healthy_ensured = tokio::time::timeout(
            Duration::from_secs(1),
            manager.ensure_core(
                &healthy_session_id,
                config.clone(),
                workspace,
                healthy_stream_tx,
                Vec::new,
            ),
        )
        .await
        .expect("其他会话的后端 Core 创建不应被故障会话阻塞")
        .expect("应创建健康会话 Core");
        assert!(healthy_ensured.is_new);

        slow_state.release();
        let (append_delivered, cleanup_result) =
            tokio::time::timeout(Duration::from_secs(2), &mut append_task)
                .await
                .expect("释放慢收尾后失败清理应结束")
                .expect("追加清理任务不应 panic");
        assert!(!append_delivered);
        cleanup_result.expect("失败清理应正常结束");
        let reopened = tokio::time::timeout(Duration::from_secs(2), &mut reopen_task)
            .await
            .expect("释放慢收尾后故障会话应能重新创建 Core")
            .expect("重新创建任务不应 panic")
            .expect("应重新创建故障会话 Core");
        assert!(reopened.is_new);
        assert!(
            !slow_state.safety_limit_reached.load(Ordering::Acquire),
            "测试应主动释放慢收尾，不应依赖 20 秒安全上限"
        );

        for cleanup_session_id in [&session_id, &healthy_session_id] {
            tokio::time::timeout(
                Duration::from_secs(2),
                manager.retire_core(cleanup_session_id, true),
            )
            .await
            .expect("测试 Core 清理不应超时")
            .expect("测试 Core 应正常清理");
        }
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
    fn agent_worker_merge_is_scoped_by_worker_and_message_id() {
        use tiangong_core::session::{Message, MessageRole, Session};

        fn worker_message(id: &str, worker_id: &str, content: &str) -> Message {
            let mut message = Message::new(MessageRole::Assistant, content);
            message.id = id.to_string();
            message.worker_id = Some(worker_id.to_string());
            message
        }

        let mut session = Session::new("desktop-agent-workers");
        let mut main = Message::new(MessageRole::Assistant, "main");
        main.id = "shared".to_string();
        session.messages.push(main);

        let cached = vec![
            worker_message("shared", "agent:dev:agent-dev", "latest"),
            worker_message("next", "agent:dev:agent-dev", "next"),
            worker_message("shared", "agent:test:agent-test", "tester"),
            worker_message("ignored", "background:worker", "ignored"),
        ];
        let authoritative_messages = session.messages.clone();
        let mut snapshot_messages = authoritative_messages.clone();
        snapshot_messages.push(worker_message("shared", "agent:dev:agent-dev", "stale"));
        merge_agent_worker_messages(&mut snapshot_messages, &cached);

        assert_eq!(session.messages.len(), authoritative_messages.len());
        assert_eq!(session.messages[0].id, authoritative_messages[0].id);
        assert_eq!(session.messages[0].text_content(), "main");
        assert!(session
            .messages
            .iter()
            .all(|message| message.worker_id.is_none()));
        assert_eq!(snapshot_messages[0].text_content(), "main");
        let workers = snapshot_messages
            .iter()
            .filter(|message| {
                message
                    .worker_id
                    .as_deref()
                    .is_some_and(|worker_id| worker_id.starts_with("agent:"))
            })
            .collect::<Vec<_>>();
        assert_eq!(workers.len(), 3);
        assert!(workers.iter().any(|message| {
            message.id == "shared"
                && message.worker_id.as_deref() == Some("agent:dev:agent-dev")
                && message.text_content() == "latest"
        }));
        assert!(workers.iter().any(|message| {
            message.id == "shared"
                && message.worker_id.as_deref() == Some("agent:test:agent-test")
                && message.text_content() == "tester"
        }));
        assert!(workers.iter().any(|message| message.id == "next"));
    }

    fn bot_store(enabled: bool) -> (tempfile::TempDir, tiangong_bots::BotStore) {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config").join("bots.json");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let config = tiangong_bots::BotsConfig {
            bots: vec![tiangong_bots::BotConfig {
                id: tiangong_bots::BotId::try_from("feishu").unwrap(),
                artifact_id: "feishu".into(),
                config: std::collections::BTreeMap::new(),
                enabled,
                created_at: "now".into(),
                updated_at: "now".into(),
            }],
        };
        std::fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
        let store = tiangong_bots::BotStore::with_config_path(config_path).unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn failed_start_state_write_triggers_stop_rollback() {
        let (dir, store) = bot_store(false);
        let id = tiangong_bots::BotId::try_from("feishu").unwrap();
        let config_dir = dir.path().join("config");
        std::fs::remove_file(config_dir.join("bots.json")).unwrap();
        std::fs::remove_dir(&config_dir).unwrap();
        std::fs::write(&config_dir, b"blocks directory creation").unwrap();
        let rollback_called = Arc::new(AtomicBool::new(false));
        let rollback_called_in_task = rollback_called.clone();

        let error = save_started_bot_state(&store, &id, || async move {
            rollback_called_in_task.store(true, Ordering::Release);
            Ok(())
        })
        .await
        .unwrap_err();

        assert!(error.contains("已撤销本次启动"));
        assert!(rollback_called.load(Ordering::Acquire));
        assert!(!store.get(&id).unwrap().enabled);
    }

    #[tokio::test]
    async fn failed_stop_restores_original_enabled_state() {
        let (_dir, store) = bot_store(true);
        let id = tiangong_bots::BotId::try_from("feishu").unwrap();

        let error = stop_bot_with_state(&store, &id, || async {
            Err("simulated stop failure".to_string())
        })
        .await
        .unwrap_err();

        assert!(error.contains("已恢复自动运行状态"));
        assert!(store.get(&id).unwrap().enabled);
    }
}

/// 按模型名从 context_windows.json 映射表解析默认 context_window（token 数）。
/// 供前端在编辑模型时预填默认值。
#[tauri::command]
pub async fn resolve_model_context_window(
    model: String,
    state: State<'_, TiangongApp>,
) -> Result<usize, String> {
    let dir = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    Ok(tiangong_config::io::resolve_context_limit_at(&dir, &model))
}

// ── 插件 UI 桥接（WASM 插件动态 UI）──
//
// 天工只提供通用桥接，不处理具体插件业务：
// - list_plugin_contributions：列出插件声明（含是否有页面）
// - plugin_open_view：按需获取插件页面 HTML（天工只当容器，不处理内容）
// - plugin_call：通用桥接，转发到 WASM 的 handle-view-message

/// 列出所有已加载 WASM 插件的设置页贡献。
#[tauri::command]
pub async fn list_plugin_contributions() -> Result<Vec<PluginContributionEntry>, String> {
    let entries = tiangong_plugin_runtime::registry::list_contributions();
    Ok(entries
        .into_iter()
        .flat_map(|(plugin_id, generation, contributions)| {
            contributions
                .into_iter()
                .map(move |c| PluginContributionEntry {
                    plugin_id: plugin_id.clone(),
                    generation,
                    contribution_id: c.id,
                    title: c.title,
                    description: c.description,
                    icon: c.icon,
                    group: c.group,
                    has_view: c.has_view,
                })
        })
        .collect())
}

/// 列出已安装插件、当前加载版本和 sidecar 状态。
#[tauri::command]
pub async fn list_plugins(
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_plugin_runtime::registry::PluginStatus>, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    tauri::async_runtime::spawn_blocking(move || {
        tiangong_plugin_runtime::registry::list_plugins(
            &storage_root,
            tiangong_plugin_runtime::registry::RuntimeKind::Desktop,
        )
    })
    .await
    .map_err(|error| format!("读取插件状态失败: {error}"))
}

/// 从 OSS 静态目录读取当前平台可安装的插件。
#[tauri::command]
pub async fn list_available_plugins(
    state: State<'_, TiangongApp>,
) -> Result<Vec<tiangong_plugin_runtime::artifacts::AvailablePlugin>, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    let repository = tiangong_plugin_runtime::artifacts::PluginRepository::new()
        .map_err(|error| error.to_string())?;
    repository
        .list_available(&storage_root)
        .await
        .map_err(|error| error.to_string())
}

/// 首次启动推荐安装检测结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DefaultPluginCheck {
    /// 是否需要弹出首次启动推荐引导（标记未完成且存在缺失的默认插件）。
    pub first_launch_pending: bool,
    /// 缺失的默认插件（OSS 目录中存在、当前平台支持且尚未安装）。
    pub missing: Vec<tiangong_plugin_runtime::artifacts::AvailablePlugin>,
    /// OSS 目录拉取失败原因。网络异常时不强弹引导，下次启动再试。
    pub catalog_error: Option<String>,
}

/// 检测是否需要首次启动推荐安装默认插件。
#[tauri::command]
pub async fn check_default_plugins(
    state: State<'_, TiangongApp>,
) -> Result<DefaultPluginCheck, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;

    // 已完成首次启动引导，直接跳过，不再打扰用户。
    if tiangong_plugin_runtime::artifacts::is_first_launch_completed(&storage_root) {
        return Ok(DefaultPluginCheck {
            first_launch_pending: false,
            missing: Vec::new(),
            catalog_error: None,
        });
    }

    let repository = tiangong_plugin_runtime::artifacts::PluginRepository::new()
        .map_err(|error| error.to_string())?;
    let available = match repository.list_available(&storage_root).await {
        Ok(list) => list,
        Err(error) => {
            // 网络或 OSS 不可达时不强弹引导，避免离线首启打扰用户。
            return Ok(DefaultPluginCheck {
                first_launch_pending: false,
                missing: Vec::new(),
                catalog_error: Some(error.to_string()),
            });
        }
    };

    let missing: Vec<_> = available
        .into_iter()
        .filter(|plugin| {
            plugin.is_default && plugin.supported && plugin.installed_version.is_none()
        })
        .collect();

    Ok(DefaultPluginCheck {
        first_launch_pending: !missing.is_empty(),
        missing,
        catalog_error: None,
    })
}

/// 标记首次启动引导已完成（用户跳过或安装结束后调用）。
#[tauri::command]
pub async fn complete_first_launch(state: State<'_, TiangongApp>) -> Result<(), String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    tauri::async_runtime::spawn_blocking(move || {
        tiangong_plugin_runtime::artifacts::mark_first_launch_completed(&storage_root)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("写入首次启动标记任务失败: {error}"))?
}

async fn download_and_install_plugin(
    storage_root: std::path::PathBuf,
    plugin_id: String,
    app: AppHandle,
) -> Result<tiangong_plugin_runtime::registry::PluginStatus, String> {
    let repository = tiangong_plugin_runtime::artifacts::PluginRepository::new()
        .map_err(|error| error.to_string())?;
    let progress: tiangong_plugin_runtime::artifacts::ProgressFn = std::sync::Arc::new({
        let app = app.clone();
        let plugin_id = plugin_id.clone();
        move |downloaded, total| {
            let _ = app.emit(
                "plugin_install_progress",
                serde_json::json!({ "plugin_id": plugin_id, "downloaded": downloaded, "total": total }),
            );
        }
    });
    let staged = repository
        .download(&storage_root, &plugin_id, Some(progress))
        .await
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        tiangong_plugin_runtime::registry::install_staged_plugin(&storage_root, staged.path())
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("安装插件任务失败: {error}"))?
}

/// 从用户选择的本地完整目录导入插件。
#[tauri::command]
pub async fn import_local_plugin(
    path: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_plugin_runtime::registry::PluginStatus, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    tauri::async_runtime::spawn_blocking(move || {
        let staged = tiangong_plugin_runtime::artifacts::stage_local_plugin(
            &storage_root,
            std::path::Path::new(&path),
        )
        .map_err(|error| error.to_string())?;
        tiangong_plugin_runtime::registry::import_staged_plugin(&storage_root, staged.path())
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("导入本地插件任务失败: {error}"))?
}

/// 从 OSS 下载并安装插件。
#[tauri::command]
pub async fn install_plugin(
    plugin_id: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_plugin_runtime::registry::PluginStatus, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    download_and_install_plugin(storage_root, plugin_id, app).await
}

/// 从 OSS 下载并升级插件，运行时负责失败恢复和本地回滚版本保留。
#[tauri::command]
pub async fn upgrade_plugin(
    plugin_id: String,
    app: AppHandle,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_plugin_runtime::registry::PluginStatus, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    download_and_install_plugin(storage_root, plugin_id, app).await
}

/// 启用或停用已安装插件。
#[tauri::command]
pub async fn set_plugin_enabled(
    plugin_id: String,
    enabled: bool,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_plugin_runtime::registry::PluginStatus, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    tauri::async_runtime::spawn_blocking(move || {
        tiangong_plugin_runtime::registry::set_plugin_enabled(&storage_root, &plugin_id, enabled)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("切换插件状态任务失败: {error}"))?
}

/// 回滚到本地保留的上一个插件版本。
#[tauri::command]
pub async fn rollback_plugin(
    plugin_id: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_plugin_runtime::registry::PluginStatus, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    tauri::async_runtime::spawn_blocking(move || {
        tiangong_plugin_runtime::registry::rollback_plugin(&storage_root, &plugin_id)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("回滚插件任务失败: {error}"))?
}

/// 卸载插件，可选择保留插件数据。
#[tauri::command]
pub async fn uninstall_plugin(
    plugin_id: String,
    keep_data: bool,
    state: State<'_, TiangongApp>,
) -> Result<(), String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    tauri::async_runtime::spawn_blocking(move || {
        tiangong_plugin_runtime::registry::uninstall_plugin(&storage_root, &plugin_id, keep_data)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("卸载插件任务失败: {error}"))?
}

/// 从已安装目录读取新制品并原子替换全部存活 WASM 实例。
#[tauri::command]
pub async fn reload_plugin(
    plugin_id: String,
    state: State<'_, TiangongApp>,
) -> Result<tiangong_plugin_runtime::registry::PluginStatus, String> {
    let storage_root = state
        .with_state_read(|core_state| Ok(core_state.config.storage_root.clone()))
        .await?;
    tauri::async_runtime::spawn_blocking(move || {
        tiangong_plugin_runtime::registry::reload_plugin(&storage_root, &plugin_id)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("热加载插件失败: {error}"))?
}

/// 按需获取插件页面 HTML（用户点击进入时才调用）。
#[tauri::command]
pub async fn plugin_open_view(
    plugin_id: String,
    contribution_id: String,
) -> Result<String, String> {
    tiangong_plugin_runtime::registry::open_view(&plugin_id, &contribution_id)
        .ok_or_else(|| format!("插件 {plugin_id} 未加载或无页面"))
}

/// 通用桥接：转发到 WASM 的 handle-view-message。
/// iframe 内的 JS 经 postMessage → 前端 → 本命令 → WASM。
/// 天工不关心 method/payload 含义，只做透传。
#[tauri::command]
pub async fn plugin_call(
    plugin_id: String,
    method: String,
    payload: String,
) -> Result<String, String> {
    tiangong_plugin_runtime::registry::handle_view_message(&plugin_id, &method, &payload)
        .ok_or_else(|| format!("插件 {plugin_id} 未加载或处理消息失败"))
}

/// 插件设置页贡献项（传给前端）。
#[derive(serde::Serialize)]
pub struct PluginContributionEntry {
    pub plugin_id: String,
    pub generation: u64,
    pub contribution_id: String,
    pub title: String,
    pub description: String,
    pub icon: String,
    pub group: String,
    pub has_view: bool,
}
