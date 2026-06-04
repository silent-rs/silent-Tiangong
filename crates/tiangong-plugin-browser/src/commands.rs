use tauri::{AppHandle, State};

use crate::types::BrowserTab;
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
    state.manager.open(&app, &url, x, y, width, height)
}

#[tauri::command]
pub async fn browser_close(state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager.close()
}

#[tauri::command]
pub async fn browser_set_position(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager.set_position(x, y)?;
    state.manager.set_size(width, height)
}

#[tauri::command]
pub async fn browser_navigate(
    url: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager.navigate(&url)
}

#[tauri::command]
pub async fn browser_eval(js: String, state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager.eval(&js)
}

#[tauri::command]
pub async fn browser_hide(state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager.hide()
}

#[tauri::command]
pub async fn browser_go_back(state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager.go_back()
}

#[tauri::command]
pub async fn browser_go_forward(state: State<'_, BrowserPluginState>) -> Result<(), String> {
    state.manager.go_forward()
}

#[tauri::command]
pub async fn browser_tab_list(
    state: State<'_, BrowserPluginState>,
) -> Result<Vec<BrowserTab>, String> {
    Ok(state.manager.tab_list())
}

#[tauri::command]
pub async fn browser_tab_new(
    url: String,
    state: State<'_, BrowserPluginState>,
) -> Result<String, String> {
    state.manager.tab_new(&url)
}

#[tauri::command]
pub async fn browser_tab_switch(
    tab_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager.tab_switch(&tab_id)
}

#[tauri::command]
pub async fn browser_tab_close(
    tab_id: String,
    state: State<'_, BrowserPluginState>,
) -> Result<(), String> {
    state.manager.tab_close(&tab_id)
}
