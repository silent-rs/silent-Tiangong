use tauri::State;

use crate::types::{TerminalSessionInfo, TerminalSessionStatus};
use crate::TerminalPluginState;

/// 等待命令循环的响应，并附加 300s 兜底超时。
///
/// 各命令内部（handle_exec 等）已有自己的超时循环，这里仅作为最后防线：
/// 若 command loop 因 Mutex 中毒等原因死锁，避免 Tauri 命令永久挂起、
/// 前端 invoke 的 Promise 永不 resolve。
async fn await_response<T: std::fmt::Debug>(
    rx: tokio::sync::oneshot::Receiver<T>,
) -> Result<T, String> {
    match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
        Ok(Ok(v)) => Ok(v),
        _ => Err("终端命令执行超时".to_string()),
    }
}

#[tauri::command]
pub async fn terminal_exec(
    command: String,
    timeout_secs: Option<u64>,
    state: State<'_, TerminalPluginState>,
) -> Result<String, String> {
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
    let resp = await_response(response_rx).await?;
    Ok(resp.stdout)
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
    await_response(response_rx).await
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
            source: crate::collaboration::InputSource::Agent,
            response_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    await_response(response_rx).await
}

#[tauri::command]
pub async fn terminal_reset(state: State<'_, TerminalPluginState>) -> Result<(), String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::Reset { response_tx })
        .await
        .map_err(|e| e.to_string())?;
    await_response(response_rx).await
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
    let _ = state
        .cmd_tx
        .send(crate::types::TerminalCommand::SetCwd { cwd })
        .await;
    Ok(())
}

#[tauri::command]
pub async fn terminal_resize(
    cols: u16,
    rows: u16,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let _ = state
        .cmd_tx
        .send(crate::types::TerminalCommand::Resize { cols, rows })
        .await;
    Ok(())
}

/// 单 PTY 模型下，会话已随系统 PTY 常驻存在，恒返回系统 PTY 存活状态。
#[tauri::command]
pub async fn terminal_ensure_session(
    _session_id: String,
    _cwd: String,
    state: State<'_, TerminalPluginState>,
) -> Result<bool, String> {
    Ok(state.manager.is_alive())
}

/// 单 PTY 模型下，销毁对话不再销毁 PTY（系统 PTY 跨对话共享）。恒为 no-op。
#[tauri::command]
pub async fn terminal_destroy_session(
    _session_id: String,
    _state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn terminal_session_send_input(
    _session_id: String,
    input: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::SendInput {
            input,
            // 面板用户输入，需记录为用户活跃来源以驱动协作状态机
            source: crate::collaboration::InputSource::User,
            response_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    await_response(response_rx).await
}

#[tauri::command]
pub async fn terminal_session_recent_output(
    _session_id: String,
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
    await_response(response_rx).await
}

#[tauri::command]
pub async fn terminal_session_info(
    _session_id: String,
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
pub async fn terminal_session_status(
    _session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<TerminalSessionStatus, String> {
    let manager = &state.manager;
    Ok(TerminalSessionStatus {
        session_id: manager.session_id(),
        alive: manager.is_alive(),
        cwd: manager.cwd(),
        shell: manager.shell(),
        phase: state.activity.busy_state().phase_label().to_string(),
    })
}

#[tauri::command]
pub async fn terminal_list_statuses(
    state: State<'_, TerminalPluginState>,
) -> Result<Vec<TerminalSessionStatus>, String> {
    // 单 PTY 模型：返回唯一的系统 PTY 条目（StatusPanel 绿点逻辑继续工作）
    let manager = &state.manager;
    Ok(vec![TerminalSessionStatus {
        session_id: manager.session_id(),
        alive: manager.is_alive(),
        cwd: manager.cwd(),
        shell: manager.shell(),
        phase: state.activity.busy_state().phase_label().to_string(),
    }])
}

#[tauri::command]
pub async fn terminal_session_set_cwd(
    _session_id: String,
    cwd: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let _ = state
        .cmd_tx
        .send(crate::types::TerminalCommand::SetCwd { cwd })
        .await;
    Ok(())
}

#[tauri::command]
pub async fn terminal_session_resize(
    _session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let _ = state
        .cmd_tx
        .send(crate::types::TerminalCommand::Resize { cols, rows })
        .await;
    Ok(())
}

#[tauri::command]
pub async fn terminal_session_reset(
    _session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    state
        .cmd_tx
        .send(crate::types::TerminalCommand::Reset { response_tx })
        .await
        .map_err(|e| e.to_string())?;
    await_response(response_rx).await
}

/// 单 PTY 模型下无需选择面板 session（系统 PTY 唯一）。保留命令签名，恒为 no-op。
#[tauri::command]
pub async fn terminal_panel_set_session(
    _session_id: Option<String>,
    _state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    Ok(())
}
