use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use tokio::sync::mpsc;

use crate::handler::browser_command_handler;
use crate::manager::BrowserManager;
use crate::session_registry::BrowserSessionRegistry;
use crate::types::{BrowserCommand, BrowserTab, BrowserTabsSnapshot};

pub mod bridge;
pub mod capability;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod page_fetcher;
pub mod plugin;
pub mod session_registry;
pub mod session_store;
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

    /// 切换 active session：隐藏旧 session 的 webview（不销毁），激活新 session，
    /// 显示其 active tab 的 webview。支持多 session webview 并发存活（T5）。
    pub fn switch_session(
        &self,
        app: &tauri::AppHandle<Wry>,
        session_id: &str,
        tabs_to_restore: Vec<BrowserTab>,
        active_tab_id: Option<String>,
    ) -> Result<BrowserTabsSnapshot, String> {
        // 1. 隐藏旧 active session 的全部 webview（不销毁，保留在各自 state 里）
        if let Some(old_id) = self.registry.active_session_id() {
            if old_id != session_id {
                if let Some(old_state) = self.registry.existing_session_state(&old_id) {
                    let old_mgr = BrowserManager::from_state(old_state);
                    let _ = old_mgr.hide();
                    // 停旧 session 轮询
                    {
                        let arc = old_mgr.clone_state();
                        let s = arc.lock().unwrap_or_else(|e| e.into_inner());
                        s.poll_stop
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        s.event_poll_stop
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }

        // 2. 激活新 session（懒创建 state），填充 tab 元数据
        let new_state = self.registry.session_state(session_id);
        self.registry.set_active(session_id);
        {
            let mut s = new_state.lock().map_err(|e| e.to_string())?;
            s.tabs = tabs_to_restore.clone();
            s.active_tab_id = active_tab_id
                .clone()
                .or_else(|| s.tabs.first().map(|t| t.id.clone()));
            s.active_session_id = Some(session_id.to_string());
            s.visible.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        // 3. 显示新 session 的 active tab webview（若缺则创建）
        let new_mgr = BrowserManager::from_state(new_state.clone());
        let (rect, active_tab) = {
            let s = new_mgr.state.lock().map_err(|e| e.to_string())?;
            let tab = s
                .active_tab_id
                .as_ref()
                .and_then(|id| s.tabs.iter().find(|t| &t.id == id).cloned());
            (s.browser_rect, tab)
        };
        if let Some(tab) = active_tab {
            let needs_webview = {
                let s = new_mgr.state.lock().map_err(|e| e.to_string())?;
                !tab.url.starts_with("about:") && !s.webviews.contains_key(&tab.id)
            };
            if needs_webview {
                let webview = BrowserManager::create_webview_for_tab(
                    app,
                    new_state.clone(),
                    &tab.id,
                    &tab.url,
                    rect.0,
                    rect.1,
                    rect.2,
                    rect.3,
                )?;
                let mut s = new_mgr.state.lock().map_err(|e| e.to_string())?;
                s.webviews.insert(tab.id.clone(), webview);
                drop(s);
                new_mgr.start_url_poll(app, &tab.url);
                new_mgr.start_event_poll(app);
            } else {
                // webview 已存在：重新定位到可见区域
                new_mgr.show_active_webview(app, &rect)?;
                new_mgr.start_url_poll(app, &tab.url);
                new_mgr.start_event_poll(app);
            }
        }

        // 4. 持久化该 session 的浏览器状态 + 返回快照
        let s = new_mgr.state.lock().map_err(|e| e.to_string())?;
        BrowserManager::persist_from_state(&s);
        Ok(BrowserTabsSnapshot {
            session_id: Some(session_id.to_string()),
            tabs: s.tabs.clone(),
            active_tab_id: s.active_tab_id.clone(),
        })
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
