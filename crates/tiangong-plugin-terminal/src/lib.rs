use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Emitter, Manager, Wry,
};

use crate::session_pty::SessionPtyRegistry;

pub mod collaboration;
pub mod command_protocol;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod output_processor;
pub mod plugin;
pub mod session_pty;
pub mod types;
pub mod util;

/// 构造终端进程内插件（issue #156 自注册架构）。
///
/// 供 main.rs setup 阶段调用，返回的 `TerminalPlugin` 通过
/// `TiangongApp::register_plugin` 注册到 app，在 core 构造时传入。
pub fn build_plugin(app: &tauri::AppHandle<Wry>) -> Option<Arc<plugin::TerminalPlugin>> {
    plugin::TerminalPlugin::from_app_handle(app).map(Arc::new)
}

/// 终端 Plugin 共享状态
pub struct TerminalPluginState {
    /// 按对话管理的 PTY 注册表（每个对话独立 PTY）
    pub registry: Arc<SessionPtyRegistry>,
}

pub fn init(session_id: String, cwd: String) -> TauriPlugin<Wry> {
    Builder::new("terminal")
        .invoke_handler(tauri::generate_handler![
            commands::terminal_ensure_session,
            commands::terminal_destroy_session,
            commands::terminal_attach_session,
            commands::terminal_session_send_input,
            commands::terminal_report_user_command,
            commands::terminal_session_recent_output,
            commands::terminal_session_info,
            commands::terminal_session_status,
            commands::terminal_list_statuses,
            commands::terminal_session_set_cwd,
            commands::terminal_session_resize,
            commands::terminal_session_reset,
            commands::terminal_session_update_screen,
            commands::terminal_panel_set_session,
        ])
        .setup(move |app, _api| {
            let app_handle: tauri::AppHandle<Wry> = app.clone();
            let registry = Arc::new(SessionPtyRegistry::new(app_handle, cwd.clone()));

            let state = TerminalPluginState {
                registry: registry.clone(),
            };
            app.manage(state);

            // 预创建初始 session 的 PTY（确保启动即可用）
            registry.ensure(&session_id, &cwd);

            Ok(())
        })
        .build()
}

/// 更新终端的默认 cwd（用于初始化同步和 workspace 切换）。
///
/// 仅影响后续懒创建的对话 PTY；已存在对话 PTY 的 cwd 由其自身管理
/// （用户在该对话内的 cd、agent 执行后的 cwd 跟踪都不受影响）。
pub async fn set_cwd(app: &tauri::AppHandle<Wry>, cwd: String) {
    let state = app.state::<TerminalPluginState>();
    state.registry.set_default_cwd(cwd);
}

/// workspace 切换时同步终端：更新默认 cwd 并销毁所有存活 PTY。
///
/// workspace 切换发生在尚未产生对话的阶段，直接销毁重建 PTY 比发送 `cd`
/// 更干净（不留历史命令、不误发到交互式程序）。用户下次打开终端时 `ensure`
/// 用新 `default_cwd` 创建全新 PTY（见 [`SessionPtyRegistry::reset_all_for_workspace`]）。
///
/// 销毁后通过 `terminal:reset` 事件通知前端丢弃 xterm 缓存并重新 `ensure`，
/// 使当前已打开的终端面板也能感知到后端 PTY 重建。
pub fn sync_workspace_cwd(app: &tauri::AppHandle<Wry>, cwd: &str) {
    let state = app.state::<TerminalPluginState>();
    state.registry.reset_all_for_workspace(cwd);
    // 通知前端：所有 PTY 已重置，丢弃 xterm 缓存后由 ensure 重建
    let _ = app.emit("terminal:reset", ());
}

/// 销毁指定对话的 PTY（删除对话时调用）
pub fn destroy_session_pty(app: &tauri::AppHandle<Wry>, session_id: &str) {
    let state = app.state::<TerminalPluginState>();
    state.registry.destroy(session_id);
}
