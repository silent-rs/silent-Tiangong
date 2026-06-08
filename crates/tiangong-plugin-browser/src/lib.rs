use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use tokio::sync::mpsc;

use crate::handler::browser_command_handler;
use crate::manager::BrowserManager;
use crate::page_fetcher::{BrowserPageFetcher, BrowserToolOverride};
use crate::types::BrowserCommand;

pub mod bridge;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod page_fetcher;
pub mod types;

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
            commands::browser_tab_list,
            commands::browser_tab_new,
            commands::browser_tab_switch,
            commands::browser_tab_close,
            commands::browser_annotation_extract,
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

/// 获取 Plugin 的 BrowserPageFetcher（用于注入到 core）
pub fn get_page_fetcher(
    app: &tauri::AppHandle<Wry>,
) -> Option<Arc<dyn tiangong_core::browser_trait::PageFetcher>> {
    let state = app.state::<BrowserPluginState>();
    Some(Arc::new(BrowserPageFetcher::new(state.cmd_tx.clone())))
}

/// 获取 Plugin 的工具覆盖处理器（用于注入到 core）
pub fn get_tool_override(
    app: &tauri::AppHandle<Wry>,
) -> Option<Arc<dyn tiangong_core::tool_override::ToolOverrideHandler>> {
    let fetcher = get_page_fetcher(app)?;
    Some(Arc::new(BrowserToolOverride::new(fetcher)))
}
