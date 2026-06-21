use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use tokio::sync::mpsc;

use crate::handler::browser_command_handler;
use crate::manager::BrowserManager;
use crate::types::BrowserCommand;

pub mod bridge;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod page_fetcher;
pub mod plugin;
pub mod types;

/// 构造浏览器进程内插件（issue #156 自注册架构）。
///
/// 供 main.rs setup 阶段调用，返回的 `BrowserPlugin` 通过
/// `TiangongApp::register_plugin` 注册到 app，在 core 构造时传入。
pub fn build_plugin(app: &tauri::AppHandle<Wry>) -> Option<Arc<plugin::BrowserPlugin>> {
    plugin::BrowserPlugin::from_app_handle(app).map(Arc::new)
}

/// 浏览器 Plugin 共享状态
pub struct BrowserPluginState {
    pub manager: BrowserManager,
    pub cmd_tx: mpsc::Sender<BrowserCommand>,
}

pub fn init() -> TauriPlugin<Wry> {
    Builder::new("browser")
        .invoke_handler(tauri::generate_handler![
            commands::browser_open,
            commands::browser_close,
            commands::browser_hide,
            commands::browser_set_position,
            commands::browser_navigate,
            commands::browser_eval,
            commands::browser_go_back,
            commands::browser_go_forward,
            commands::browser_set_zoom,
            commands::browser_get_zoom,
            commands::browser_reset_zoom,
            commands::browser_tab_list,
            commands::browser_tab_new,
            commands::browser_tab_switch,
            commands::browser_tab_close,
            commands::browser_annotation_extract,
            commands::browser_tab_history,
            commands::browser_global_history,
            commands::browser_global_history_clear,
            commands::browser_global_history_delete,
        ])
        .setup(|app, _api| {
            let (tx, rx) = mpsc::channel::<BrowserCommand>(16);
            let manager = BrowserManager::new();
            let state = BrowserPluginState {
                manager,
                cmd_tx: tx,
            };
            app.manage(state);

            let browser_state = app.state::<BrowserPluginState>();
            let browser_manager_state = browser_state.manager.clone_state();
            let app_handle: tauri::AppHandle<Wry> = app.clone();

            tauri::async_runtime::spawn(browser_command_handler(
                rx,
                browser_manager_state,
                app_handle,
            ));

            Ok(())
        })
        .build()
}
