use std::sync::Arc;

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Wry,
};
use tokio::sync::mpsc;
use tracing::error;

use crate::handler::{TerminalPromptSectionProvider, TerminalProviderImpl, TerminalToolOverride};
use crate::manager::{spawn_command_loop, TerminalManager};

pub mod collaboration;
pub mod command_protocol;
pub mod commands;
pub mod handler;
pub mod manager;
pub mod output_processor;
pub mod types;
pub mod util;

/// 系统 PTY 日志回填的最大行数
const DEFAULT_LOG_TAIL_LINES: usize = 5000;

/// 系统 PTY 持久化日志路径：`~/.tiangong/terminal.log`。
/// 与应用数据目录（tiangong-config 的 default_tiangong_dir）保持一致。
fn terminal_log_path() -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home)
        .join(".tiangong")
        .join("terminal.log")
}

/// 终端 Plugin 共享状态
pub struct TerminalPluginState {
    /// 系统 PTY（agent 工具执行 + 面板共用，单 PTY 模型）
    pub manager: Arc<TerminalManager>,
    pub cmd_tx: mpsc::Sender<types::TerminalCommand>,
    /// 协作状态跟踪器（用户/Agent 协作状态机，系统 PTY 共享）
    pub activity: Arc<crate::collaboration::TerminalActivityTracker>,
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
            let mut manager = TerminalManager::new(session_id.clone(), cwd.clone());

            // 打开系统 PTY 持久化日志，回填历史到环形缓冲区（实现「终端历史保留」）
            let log_path = terminal_log_path();
            if let Some(logger) = output_processor::OutputLogger::open(log_path) {
                let tail = output_processor::read_log_tail(logger.path(), DEFAULT_LOG_TAIL_LINES);
                if !tail.is_empty() {
                    let mut state = manager.state.lock().unwrap();
                    for line in &tail {
                        output_processor::backfill_line(&mut state, line.clone());
                    }
                    drop(state);
                }
                manager.set_logger(Arc::new(logger));
            }

            let manager = Arc::new(manager);

            // 启动系统 PTY（agent 工具执行 + 面板共用）
            let app_handle: tauri::AppHandle<Wry> = app.clone();
            let pty_state = manager.start_and_spawn_reader(&session_id, &cwd, app_handle);

            if pty_state.is_none() {
                error!(session_id = %session_id, "系统 PTY 启动失败");
            }

            // 协作状态跟踪器提升到系统级：单 PTY 下用户和 Agent 共享同一状态机
            let activity = Arc::new(crate::collaboration::TerminalActivityTracker::new());

            let state = TerminalPluginState {
                manager: manager.clone(),
                cmd_tx: tx,
                activity: activity.clone(),
            };
            app.manage(state);

            let app_handle: tauri::AppHandle<Wry> = app.clone();
            // 系统 PTY 命令循环携带 activity，使 command_protocol 里的协作状态机生效
            tauri::async_runtime::spawn(spawn_command_loop(
                rx,
                manager,
                app_handle,
                pty_state,
                Some(activity),
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
    Some(Arc::new(TerminalProviderImpl::new(state.cmd_tx.clone())))
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

/// 更新系统终端的工作目录
pub async fn set_cwd(app: &tauri::AppHandle<Wry>, cwd: String) {
    let state = app.state::<TerminalPluginState>();
    let _ = state
        .cmd_tx
        .send(types::TerminalCommand::SetCwd { cwd })
        .await;
}

/// 单 PTY 模型下销毁对话不再清理 PTY（系统 PTY 跨对话共享）。
/// 保留函数签名避免外部调用方（src-tauri delete_session）改动，现为 no-op。
pub fn destroy_session_pty(_app: &tauri::AppHandle<Wry>, _session_id: &str) {}
