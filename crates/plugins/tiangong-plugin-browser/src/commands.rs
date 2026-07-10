use tauri::{AppHandle, Emitter, State};

use crate::manager::BrowserManager;
use crate::types::{
    AnnotationExtractResult, BrowserTabsSnapshot, HistoryEntry, TabHistoryResult, TabListResponse,
};
use crate::BrowserPluginState;

/// 按 session_id 获取绑定到该 session 的 manager（不 fallback active/bootstrap）。
/// 所有 browser UI 命令必须显式传 session_id。
fn session_manager(state: &BrowserPluginState, session_id: &str) -> Result<BrowserManager, String> {
    if session_id.trim().is_empty() {
        return Err("browser session_id 不能为空".to_string());
    }
    Ok(BrowserManager::from_state(
        state.registry.session_state(session_id),
    ))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn browser_open(
    session_id: String,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.open(&app, &url, x, y, width, height)
}

#[tauri::command]
pub async fn browser_close(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.close()
}

#[tauri::command]
pub async fn browser_set_position(
    session_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    let mgr = session_manager(&state, &session_id)?;
    mgr.set_position(x, y)?;
    mgr.set_size(width, height)
}

#[tauri::command]
pub async fn browser_navigate(
    session_id: String,
    url: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.navigate_with_app(&app, &url)?;
    let _ = app.emit("browser:tab_updated", ());
    Ok(())
}

/// 原子打开 URL：校验 session → 导航 → 持久化 → 返回 tab 快照。
/// 供前端链接点击使用，避免"打开面板 + 等一帧 + navigate"的竞态。
#[tauri::command]
pub async fn browser_open_url(
    session_id: String,
    url: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<BrowserTabsSnapshot, String> {
    let mgr = session_manager(&state, &session_id)?;
    mgr.navigate_with_app(&app, &url)?;
    mgr.persist_session_tabs();
    Ok(mgr.snapshot_tabs())
}

#[tauri::command]
pub async fn browser_eval(
    session_id: String,
    js: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.eval(&js)
}

#[tauri::command]
pub async fn browser_hide(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.hide()
}

#[tauri::command]
pub async fn browser_go_back(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.go_back()
}

#[tauri::command]
pub async fn browser_go_forward(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.go_forward()
}

#[tauri::command]
pub async fn browser_set_zoom(
    session_id: String,
    scale: f64,
    state: State<'_, BrowserPluginState>,
) -> Result<f64, String> {
    session_manager(&state, &session_id)?.set_zoom(scale)
}

#[tauri::command]
pub async fn browser_get_zoom(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<f64, String> {
    Ok(session_manager(&state, &session_id)?.zoom())
}

#[tauri::command]
pub async fn browser_reset_zoom(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<f64, String> {
    session_manager(&state, &session_id)?.reset_zoom()
}

#[tauri::command]
pub async fn browser_tab_list(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<TabListResponse, String> {
    Ok(session_manager(&state, &session_id)?.tab_list_with_active())
}

#[tauri::command]
pub async fn browser_snapshot_tabs(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<BrowserTabsSnapshot, String> {
    Ok(session_manager(&state, &session_id)?.snapshot_tabs())
}

/// 切换 active session：后端从 BrowserSessionStore 恢复，不接受前端 tabs 覆盖。
#[tauri::command]
pub async fn browser_switch_session(
    session_id: String,
    active_tab_id: Option<String>,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<BrowserTabsSnapshot, String> {
    let snapshot = state.switch_session(&app, &session_id, active_tab_id)?;
    let _ = app.emit(
        "browser:tab_updated",
        serde_json::json!({
            "action": "switch_session",
            "session_id": session_id,
            "active_tab_id": snapshot.active_tab_id.clone(),
        }),
    );
    Ok(snapshot)
}

#[tauri::command]
pub async fn browser_tab_new(
    session_id: String,
    url: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<String, String> {
    let result = session_manager(&state, &session_id)?.tab_new(&app, &url);
    let _ = app.emit("browser:tab_updated", ());
    result
}

#[tauri::command]
pub async fn browser_tab_switch(
    session_id: String,
    tab_id: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.tab_switch(&tab_id)?;
    let _ = app.emit("browser:tab_updated", ());
    Ok(())
}

#[tauri::command]
pub async fn browser_tab_close(
    session_id: String,
    tab_id: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    session_manager(&state, &session_id)?.tab_close(&tab_id)?;
    let _ = app.emit("browser:tab_updated", ());
    Ok(())
}

#[tauri::command]
pub async fn browser_annotation_extract(
    session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<AnnotationExtractResult, String> {
    if session_id.trim().is_empty() {
        return Err("browser session_id 不能为空".to_string());
    }
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::BrowserCommand::AnnotationExtract {
            session_id,
            response_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(10), response_rx)
        .await
        .map_err(|_| "提取批注元素超时".to_string())?
        .map_err(|_| "提取批注元素响应失败".to_string())
}

#[tauri::command]
pub async fn browser_tab_history(
    session_id: String,
    tab_id: Option<String>,
    state: State<'_, BrowserPluginState>,
) -> Result<TabHistoryResult, String> {
    Ok(session_manager(&state, &session_id)?
        .get_tab_history(tab_id.as_deref())
        .unwrap_or(TabHistoryResult {
            tab_id: String::new(),
            entries: Vec::new(),
            current_index: -1,
        }))
}

#[tauri::command]
pub async fn browser_global_history(
    offset: usize,
    limit: usize,
    state: State<'_, BrowserPluginState>,
) -> Result<Vec<HistoryEntry>, String> {
    // 全局历史是进程级共享，仍用 active session 的 state 读
    Ok(state.manager().get_global_history(offset, limit))
}

#[tauri::command]
pub async fn browser_global_history_clear(
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager().clear_global_history();
    Ok(())
}

#[tauri::command]
pub async fn browser_global_history_delete(
    url: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager().delete_global_history_entry(&url);
    Ok(())
}

/// 草稿 session 转正：迁移 BrowserState 的 registry key + 持久化文件。
#[tauri::command]
pub async fn browser_attach_session(
    draft_session_id: String,
    persistent_session_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state
        .registry
        .attach_session(&draft_session_id, &persistent_session_id);
    Ok(())
}
