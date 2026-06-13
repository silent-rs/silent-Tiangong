use std::sync::Arc;

use tauri::State;

use crate::registry::SessionSlot;
use crate::types::TerminalSessionInfo;
use crate::TerminalPluginState;

// ===== 系统 PTY 命令（agent 工具执行用）=====

#[tauri::command]
pub async fn terminal_exec(
    command: String,
    timeout_secs: Option<u64>,
    state: State<'_, TerminalPluginState>,
) -> Result<crate::types::TerminalExecResponse, String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::Exec {
            command,
            timeout_secs,
            response_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(180), response_rx)
        .await
        .map_err(|_| "终端命令执行超时".to_string())?
        .map_err(|_| "终端命令执行响应失败".to_string())
}

#[tauri::command]
pub async fn terminal_recent_output(
    lines: Option<usize>,
    state: State<'_, TerminalPluginState>,
) -> Result<String, String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::RecentOutput {
            lines: lines.unwrap_or(50),
            response_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(5), response_rx)
        .await
        .map_err(|_| "获取终端输出超时".to_string())?
        .map_err(|_| "获取终端输出响应失败".to_string())
}

#[tauri::command]
pub async fn terminal_send_input(
    input: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::SendInput {
            input,
            source: crate::collaboration::InputSource::User,
            response_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(5), response_rx)
        .await
        .map_err(|_| "发送终端输入超时".to_string())?
        .map_err(|_| "发送终端输入响应失败".to_string())
}

#[tauri::command]
pub async fn terminal_reset(state: State<'_, TerminalPluginState>) -> Result<(), String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::Reset { response_tx })
        .await
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(10), response_rx)
        .await
        .map_err(|_| "终端重置超时".to_string())?
        .map_err(|_| "终端重置响应失败".to_string())
}

#[tauri::command]
pub async fn terminal_system_session_info(
    state: State<'_, TerminalPluginState>,
) -> Result<TerminalSessionInfo, String> {
    let manager = &state.manager;
    Ok(TerminalSessionInfo {
        session_id: manager.session_id(),
        cwd: manager.cwd(),
        shell: manager.shell(),
        alive: manager.is_alive(),
    })
}

#[tauri::command]
pub async fn terminal_set_cwd(
    cwd: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::SetCwd { cwd })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn terminal_resize(
    cols: u16,
    rows: u16,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::Resize { cols, rows })
        .await
        .map_err(|e| e.to_string())
}

// ===== 交互 PTY 命令（按对话独立）=====

fn get_slot(
    session_id: &str,
    state: &State<'_, TerminalPluginState>,
) -> Result<Arc<SessionSlot>, String> {
    state
        .registry
        .get_slot(session_id)
        .ok_or_else(|| format!("终端会话 {} 不存在", session_id))
}

#[tauri::command]
pub async fn terminal_ensure_session(
    session_id: String,
    cwd: String,
    state: State<'_, TerminalPluginState>,
) -> Result<bool, String> {
    Ok(state.registry.ensure_slot(&session_id, &cwd))
}

#[tauri::command]
pub async fn terminal_destroy_session(
    session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    state.registry.destroy_slot(&session_id);
    Ok(())
}

#[tauri::command]
pub async fn terminal_session_send_input(
    session_id: String,
    input: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let slot = get_slot(&session_id, &state)?;
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    slot.cmd_tx
        .send(crate::types::TerminalCommand::SendInput {
            input,
            source: crate::collaboration::InputSource::User,
            response_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(5), response_rx)
        .await
        .map_err(|_| "发送终端输入超时".to_string())?
        .map_err(|_| "发送终端输入响应失败".to_string())
}

#[tauri::command]
pub async fn terminal_session_recent_output(
    session_id: String,
    lines: Option<usize>,
    state: State<'_, TerminalPluginState>,
) -> Result<String, String> {
    let slot = get_slot(&session_id, &state)?;
    Ok(slot.manager.recent_output(lines.unwrap_or(50)))
}

#[tauri::command]
pub async fn terminal_session_info(
    session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<TerminalSessionInfo, String> {
    let slot = get_slot(&session_id, &state)?;
    let manager = &slot.manager;
    Ok(TerminalSessionInfo {
        session_id: manager.session_id(),
        cwd: manager.cwd(),
        shell: manager.shell(),
        alive: manager.is_alive(),
    })
}

#[tauri::command]
pub async fn terminal_session_set_cwd(
    session_id: String,
    cwd: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let slot = get_slot(&session_id, &state)?;
    slot.cmd_tx
        .send(crate::types::TerminalCommand::SetCwd { cwd })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn terminal_session_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let slot = get_slot(&session_id, &state)?;
    slot.cmd_tx
        .send(crate::types::TerminalCommand::Resize { cols, rows })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn terminal_session_reset(
    session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let slot = get_slot(&session_id, &state)?;
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    slot.cmd_tx
        .send(crate::types::TerminalCommand::Reset { response_tx })
        .await
        .map_err(|e| e.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(10), response_rx)
        .await
        .map_err(|_| "终端重置超时".to_string())?
        .map_err(|_| "终端重置响应失败".to_string())
}

// ===== 面板状态命令 =====

#[tauri::command]
pub async fn terminal_panel_set_session(
    session_id: Option<String>,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    state.registry.set_panel_session(session_id.as_deref());
    Ok(())
}
