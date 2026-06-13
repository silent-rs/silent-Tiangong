use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use tokio::sync::mpsc;
use tracing::error;

use crate::handler::{
    TerminalPromptSectionProvider, TerminalProviderImpl, TerminalToolOverride,
    TerminalToolSpecProvider,
};
use crate::manager::{spawn_command_loop, TerminalManager};
use crate::registry::TerminalSessionRegistry;

pub mod collaboration;
pub mod command_protocol;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod output_processor;
pub mod registry;
pub mod types;
pub mod util;

/// 终端 Plugin 共享状态
pub struct TerminalPluginState {
    /// 系统 PTY（agent 工具执行用）
    pub manager: Arc<TerminalManager>,
    pub cmd_tx: mpsc::Sender<types::TerminalCommand>,
    /// 交互 PTY 注册表（按对话独立）
    pub registry: Arc<TerminalSessionRegistry>,
}

pub fn init(session_id: String, cwd: String) -> TauriPlugin<Wry> {
    Builder::new("terminal")
        .invoke_handler(tauri::generate_handler![
            commands::terminal_exec,
            commands::terminal_recent_output,
            commands::terminal_send_input,
            commands::terminal_reset,
            commands::terminal_system_session_info,
            commands::terminal_set_cwd,
            commands::terminal_resize,
            commands::terminal_ensure_session,
            commands::terminal_destroy_session,
            commands::terminal_session_send_input,
            commands::terminal_session_recent_output,
            commands::terminal_session_info,
            commands::terminal_session_status,
            commands::terminal_list_statuses,
            commands::terminal_session_set_cwd,
            commands::terminal_session_resize,
            commands::terminal_session_reset,
            commands::terminal_panel_set_session,
        ])
        .setup(move |app, _api| {
            let (tx, rx) = mpsc::channel::<types::TerminalCommand>(16);
            let manager = Arc::new(TerminalManager::new(session_id.clone(), cwd.clone()));

            // 启动系统 PTY（agent 工具执行用）
            let app_handle: tauri::AppHandle<Wry> = app.clone();
            let pty_state = manager.start_and_spawn_reader(&session_id, &cwd, app_handle);

            if pty_state.is_none() {
                error!(session_id = %session_id, "系统 PTY 启动失败");
            }

            let registry = Arc::new(TerminalSessionRegistry::new(app.clone(), cwd.clone()));

            let state = TerminalPluginState {
                manager: manager.clone(),
                cmd_tx: tx,
                registry,
            };
            app.manage(state);

            let app_handle: tauri::AppHandle<Wry> = app.clone();
            tauri::async_runtime::spawn(spawn_command_loop(
                rx, manager, app_handle, pty_state, None,
            ));

            Ok(())
        })
        .build()
}

/// 获取 Plugin 的 TerminalProvider（用于注入到 core）
pub fn get_terminal_provider(
    app: &tauri::AppHandle<Wry>,
) -> Option<Arc<dyn tiangong_core::terminal_trait::TerminalProvider>> {
    let state = app.state::<TerminalPluginState>();
    Some(Arc::new(TerminalProviderImpl::new(
        state.cmd_tx.clone(),
        state.registry.clone(),
    )))
}

/// 获取 Plugin 的工具覆盖处理器（用于注入到 core）
pub fn get_tool_override(
    app: &tauri::AppHandle<Wry>,
) -> Option<Arc<dyn tiangong_core::tool_override::ToolOverrideHandler>> {
    let provider = get_terminal_provider(app)?;
    Some(Arc::new(TerminalToolOverride::new(provider)))
}

/// 获取 Plugin 的工具规格提供者（用于注册到 core）
pub fn get_tool_spec_provider() -> Arc<dyn tiangong_core::tool_override::ToolSpecProvider> {
    Arc::new(TerminalToolSpecProvider)
}

/// 获取 Plugin 的 Prompt 规则提供者（用于注册到 core）
pub fn get_prompt_section_provider() -> Arc<dyn tiangong_core::tool_override::PromptSectionProvider>
{
    Arc::new(TerminalPromptSectionProvider)
}

/// 更新系统终端的工作目录
pub async fn set_cwd(app: &tauri::AppHandle<Wry>, cwd: String) {
    let state = app.state::<TerminalPluginState>();
    let _ = state
        .cmd_tx
        .send(types::TerminalCommand::SetCwd { cwd })
        .await;
}

/// 销毁指定对话的交互 PTY
pub fn destroy_session_pty(app: &tauri::AppHandle<Wry>, session_id: &str) {
    let state = app.state::<TerminalPluginState>();
    state.registry.destroy_slot(session_id);
}
