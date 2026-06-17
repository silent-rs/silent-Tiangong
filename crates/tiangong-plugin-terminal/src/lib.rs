use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};

use crate::handler::{TerminalPromptSectionProvider, TerminalToolOverride};
use crate::session_pty::SessionPtyRegistry;

pub mod collaboration;
pub mod command_protocol;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod output_processor;
pub mod session_pty;
pub mod types;
pub mod util;

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

/// 获取 Plugin 的 TerminalProvider（用于注入到 core）
pub fn get_terminal_provider(
    app: &tauri::AppHandle<Wry>,
) -> Option<Arc<dyn tiangong_core::terminal_trait::TerminalProvider>> {
    let state = app.state::<TerminalPluginState>();
    Some(Arc::new(
        crate::session_pty::SessionAwareTerminalProvider::new(state.registry.clone()),
    ))
}

/// 获取 Plugin 的工具覆盖处理器（用于注入到 core）
pub fn get_tool_override(
    app: &tauri::AppHandle<Wry>,
) -> Option<Arc<dyn tiangong_core::tool_override::ToolOverrideHandler>> {
    let provider = get_terminal_provider(app)?;
    Some(Arc::new(TerminalToolOverride::new(provider)))
}

/// 获取 Plugin 的 Prompt 规则提供者（用于注册到 core）
pub fn get_prompt_section_provider() -> Arc<dyn tiangong_core::tool_override::PromptSectionProvider>
{
    Arc::new(TerminalPromptSectionProvider)
}

/// 更新终端的默认 cwd（用于初始化同步和 workspace 切换）。
///
/// 仅影响后续懒创建的对话 PTY；已存在对话 PTY 的 cwd 由其自身管理
/// （用户在该对话内的 cd、agent 执行后的 cwd 跟踪都不受影响）。
pub async fn set_cwd(app: &tauri::AppHandle<Wry>, cwd: String) {
    let state = app.state::<TerminalPluginState>();
    state.registry.set_default_cwd(cwd);
}

/// 销毁指定对话的 PTY（删除对话时调用）
pub fn destroy_session_pty(app: &tauri::AppHandle<Wry>, session_id: &str) {
    let state = app.state::<TerminalPluginState>();
    state.registry.destroy(session_id);
}
