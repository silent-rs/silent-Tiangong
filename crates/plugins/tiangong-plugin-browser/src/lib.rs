use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use tokio::sync::mpsc;

use crate::handler::browser_command_handler;
use crate::manager::BrowserManager;
use crate::session_registry::BrowserSessionRegistry;
use crate::types::BrowserCommand;

pub mod bridge;
pub mod capability;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod page_fetcher;
pub mod plugin;
pub mod session_registry;
pub mod types;
pub mod watcher;

/// 构造浏览器进程内插件（issue #156 自注册架构）。
///
/// 供 main.rs setup 阶段调用，返回的 `BrowserPlugin` 通过
/// `TiangongApp::register_plugin` 注册到 app，在 core 构造时传入。
pub fn build_plugin(app: &tauri::AppHandle<Wry>) -> Option<Arc<plugin::BrowserPlugin>> {
    plugin::BrowserPlugin::from_app_handle(app).map(Arc::new)
}

/// 浏览器 Plugin 共享状态
///
/// `registry` 持有所有 session 的 `BrowserState`；`manager` 绑定到当前 active session，
/// 前端命令经它操作（行为同旧的全局单例）。切换 session 时经 registry 切换 active，
/// manager 重新绑定（T5 实现；当前 manager 绑定首个注册 session 兼容旧路径）。
pub struct BrowserPluginState {
    pub registry: Arc<BrowserSessionRegistry>,
    pub cmd_tx: mpsc::Sender<BrowserCommand>,
}

impl BrowserPluginState {
    /// 返回绑定到当前 active session 的 manager。
    ///
    /// 前端命令经此获取 manager（行为同旧的全局单例）。每次调用都取最新 active session。
    /// 无 active session 时懒创建一个 bootstrap session（兼容早期启动）。
    pub fn manager(&self) -> BrowserManager {
        let state = self
            .registry
            .active_state()
            .unwrap_or_else(|| self.registry.session_state("__bootstrap__"));
        BrowserManager::from_state(state)
    }
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
            commands::browser_snapshot_tabs,
            commands::browser_switch_session,
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
            let registry = Arc::new(BrowserSessionRegistry::new());
            let state = BrowserPluginState {
                registry,
                cmd_tx: tx,
            };
            app.manage(state);

            let browser_state = app.state::<BrowserPluginState>();
            let browser_registry = browser_state.registry.clone();
            let app_handle: tauri::AppHandle<Wry> = app.clone();

            tauri::async_runtime::spawn(browser_command_handler(rx, browser_registry, app_handle));

            Ok(())
        })
        .build()
}
