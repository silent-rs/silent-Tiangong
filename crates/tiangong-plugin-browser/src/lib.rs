use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use tokio::sync::mpsc;

use crate::handler::browser_command_handler;
use crate::manager::BrowserManager;
use crate::page_fetcher::{BrowserPageFetcher, BrowserToolOverride};
use crate::types::{BrowserCommand, BrowserEvent};
use crate::watcher::run_browser_watcher;

pub mod bridge;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod page_fetcher;
pub mod types;
pub mod watcher;

/// 浏览器 Plugin 共享状态
pub struct BrowserPluginState {
    pub manager: BrowserManager,
    pub cmd_tx: mpsc::Sender<BrowserCommand>,
    pub event_tx: mpsc::Sender<BrowserEvent>,
    event_rx: std::sync::Mutex<Option<mpsc::Receiver<BrowserEvent>>>,
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
        ])
        .setup(|app, _api| {
            let (cmd_tx, cmd_rx) = mpsc::channel::<BrowserCommand>(16);
            let (event_tx, event_rx) = mpsc::channel::<BrowserEvent>(32);
            let manager = BrowserManager::new();
            let state = BrowserPluginState {
                manager,
                cmd_tx: cmd_tx.clone(),
                event_tx: event_tx.clone(),
                event_rx: std::sync::Mutex::new(Some(event_rx)),
            };
            app.manage(state);

            // 将 event_tx 注入到 BrowserState，供 on_page_load 回调使用
            {
                let browser_state = app.state::<BrowserPluginState>();
                let mut s = browser_state
                    .manager
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                s.event_tx = Some(event_tx.clone());
            }

            let browser_state = app.state::<BrowserPluginState>();
            let browser_manager_state = browser_state.manager.clone_state();
            let app_handle: tauri::AppHandle<Wry> = app.clone();

            // 命令处理任务
            tauri::async_runtime::spawn(browser_command_handler(
                cmd_rx,
                browser_manager_state.clone(),
                app_handle.clone(),
                event_tx.clone(),
            ));

            // 浏览器监测任务（常驻）
            let watcher_stop = {
                let s = browser_state
                    .manager
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                s.watcher_stop.clone()
            };
            tauri::async_runtime::spawn(run_browser_watcher(
                browser_manager_state,
                cmd_tx,
                event_tx,
                watcher_stop,
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

/// 取出浏览器事件接收端（仅可调用一次）
pub fn take_event_rx(app: &tauri::AppHandle<Wry>) -> Option<mpsc::Receiver<BrowserEvent>> {
    let state = app.state::<BrowserPluginState>();
    state.event_rx.lock().ok().and_then(|mut rx| rx.take())
}
