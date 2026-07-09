use tauri::{AppHandle, Emitter, State};

use crate::types::{
    AnnotationExtractResult, BrowserTab, BrowserTabsSnapshot, HistoryEntry, TabHistoryResult,
    TabListResponse,
};
use crate::BrowserPluginState;

#[tauri::command]
pub async fn browser_open(
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager().open(&app, &url, x, y, width, height)
}

#[tauri::command]
pub async fn browser_close(state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager().close()
}

#[tauri::command]
pub async fn browser_set_position(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager().set_position(x, y)?;
    state.manager().set_size(width, height)
}

#[tauri::command]
pub async fn browser_navigate(
    url: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager().navigate_with_app(&app, &url)?;
    let _ = app.emit("browser:tab_updated", ());
    Ok(())
}

#[tauri::command]
pub async fn browser_eval(js: String, state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager().eval(&js)
}

#[tauri::command]
pub async fn browser_hide(state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager().hide()
}

#[tauri::command]
pub async fn browser_go_back(state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager().go_back()
}

#[tauri::command]
pub async fn browser_go_forward(state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager().go_forward()
}

#[tauri::command]
pub async fn browser_set_zoom(
    scale: f64,
    state: State<'_, BrowserPluginState>,
) -> Result<f64, String> {
    state.manager().set_zoom(scale)
}

#[tauri::command]
pub async fn browser_get_zoom(state: State<'_, BrowserPluginState>) -> Result<f64, String> {
    Ok(state.manager().zoom())
}

#[tauri::command]
pub async fn browser_reset_zoom(state: State<'_, BrowserPluginState>) -> Result<f64, String> {
    state.manager().reset_zoom()
}

#[tauri::command]
pub async fn browser_tab_list(
    state: State<'_, BrowserPluginState>,
) -> Result<TabListResponse, String> {
    Ok(state.manager().tab_list_with_active())
}

#[tauri::command]
pub async fn browser_snapshot_tabs(
    state: State<'_, BrowserPluginState>,
) -> Result<BrowserTabsSnapshot, String> {
    Ok(state.manager().snapshot_tabs())
}

#[tauri::command]
pub async fn browser_switch_session(
    session_id: String,
    tabs_to_restore: Vec<BrowserTab>,
    active_tab_id: Option<String>,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<BrowserTabsSnapshot, String> {
    let snapshot = state.switch_session(&app, &session_id, tabs_to_restore, active_tab_id)?;
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
    url: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<String, String> {
    let result = state.manager().tab_new(&app, &url);
    let _ = app.emit("browser:tab_updated", ());
    result
}

#[tauri::command]
pub async fn browser_tab_switch(
    tab_id: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager().tab_switch(&tab_id)?;
    let _ = app.emit("browser:tab_updated", ());
    Ok(())
}

#[tauri::command]
pub async fn browser_tab_close(
    tab_id: String,
    app: AppHandle,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager().tab_close(&tab_id)?;
    let _ = app.emit("browser:tab_updated", ());
    Ok(())
}

#[tauri::command]
pub async fn browser_annotation_extract(
    state: State<'_, BrowserPluginState>,
) -> Result<AnnotationExtractResult, String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::BrowserCommand::AnnotationExtract {
            session_id: state.registry.active_session_id().unwrap_or_default(),
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
    tab_id: Option<String>,
    state: State<'_, BrowserPluginState>,
) -> Result<TabHistoryResult, String> {
    Ok(state
        .manager()
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
