use tauri::{Emitter, State};

use crate::session_pty::SessionPty;
use crate::types::{TerminalSessionInfo, TerminalSessionStatus};
use crate::TerminalPluginState;

/// 等待命令循环的响应，并附加 300s 兜底超时。
async fn await_response<T: std::fmt::Debug>(
    rx: tokio::sync::oneshot::Receiver<T>,
) -> Result<T, String> {
    match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
        Ok(Ok(v)) => Ok(v),
        _ => Err("终端命令执行超时".to_string()),
    }
}

/// 获取指定 session 的 PTY，不存在则用 default cwd 懒创建。
///
/// 正常流程下前端会先调 `terminal_ensure_session`，但 agent 执行后直接查状态、
/// 或面板恢复时跳过 ensure 的情况下，这里兜底懒创建，避免直接报错。
fn ensure_pty(
    state: &State<'_, TerminalPluginState>,
    session_id: &str,
) -> Result<SessionPty, String> {
    if state.registry.get(session_id).is_none() {
        state.registry.ensure(session_id, "");
    }
    state
        .registry
        .get(session_id)
        .ok_or(format!("终端会话 {session_id} 创建失败"))
}

// ===== 按对话 session 命令（按 session_id 路由到对应对话的 PTY）=====

/// 懒创建指定 session 的 PTY，返回存活状态。
#[tauri::command]
pub async fn terminal_ensure_session(
    session_id: String,
    cwd: String,
    state: State<'_, TerminalPluginState>,
) -> Result<bool, String> {
    Ok(state.registry.ensure(&session_id, &cwd))
}

/// 销毁指定 session 的 PTY。
#[tauri::command]
pub async fn terminal_destroy_session(
    session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    state.registry.destroy(&session_id);
    Ok(())
}

/// 把草稿态临时 id 的 PTY 迁移到真实 session_id（草稿态转正时调用）。
///
/// 草稿态新对话用稳定临时 id 创建 PTY；首条消息创建后端 session 拿到真实 id 后，
/// 调用此命令把 PTY 归属、日志迁移到真实 id。幂等：草稿 id 不存在或真实 id 已
/// 存在时安全返回。
#[tauri::command]
pub async fn terminal_attach_session(
    draft_session_id: String,
    persistent_session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    state
        .registry
        .attach_persistent_session_id(&draft_session_id, &persistent_session_id);
    Ok(())
}

#[tauri::command]
pub async fn terminal_session_send_input(
    session_id: String,
    input: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let pty = ensure_pty(&state, &session_id)?;
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    pty.cmd_tx
        .send(crate::types::TerminalCommand::SendInput {
            input,
            source: crate::collaboration::InputSource::User,
            response_tx,
        })
        .await
        .map_err(|e| e.to_string())?;
    await_response(response_rx).await
}

/// 上报用户在终端提交的完整命令行（回车截断后由前端累积上报）。
/// emit `terminal:user_command` 事件，供 main.rs 监听后注入 Agent 对话链。
#[tauri::command]
pub async fn terminal_report_user_command(
    session_id: String,
    command: String,
    state: State<'_, TerminalPluginState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let pty = ensure_pty(&state, &session_id)?;
    pty.activity.record_user_command(command.clone());
    let _ = app_handle.emit(
        "terminal:user_command",
        serde_json::json!({ "session_id": session_id, "command": command }),
    );
    Ok(())
}

#[tauri::command]
pub async fn terminal_session_recent_output(
    session_id: String,
    lines: Option<usize>,
    state: State<'_, TerminalPluginState>,
) -> Result<String, String> {
    let pty = ensure_pty(&state, &session_id)?;
    Ok(pty.manager.recent_output(lines.unwrap_or(50)))
}

#[tauri::command]
pub async fn terminal_session_info(
    session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<TerminalSessionInfo, String> {
    let pty = ensure_pty(&state, &session_id)?;
    let manager = &pty.manager;
    Ok(TerminalSessionInfo {
        session_id: manager.session_id(),
        cwd: manager.cwd(),
        shell: manager.shell(),
        alive: manager.is_alive(),
    })
}

#[tauri::command]
pub async fn terminal_session_status(
    session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<TerminalSessionStatus, String> {
    let pty = ensure_pty(&state, &session_id)?;
    let manager = &pty.manager;
    Ok(TerminalSessionStatus {
        session_id: manager.session_id(),
        alive: manager.is_alive(),
        cwd: manager.cwd(),
        shell: manager.shell(),
        phase: pty.activity.busy_state().phase_label().to_string(),
    })
}

#[tauri::command]
pub async fn terminal_list_statuses(
    state: State<'_, TerminalPluginState>,
) -> Result<Vec<TerminalSessionStatus>, String> {
    Ok(state.registry.list_statuses())
}

#[tauri::command]
pub async fn terminal_session_set_cwd(
    session_id: String,
    cwd: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let pty = ensure_pty(&state, &session_id)?;
    let _ = pty
        .cmd_tx
        .send(crate::types::TerminalCommand::SetCwd { cwd })
        .await;
    Ok(())
}

#[tauri::command]
pub async fn terminal_session_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let pty = ensure_pty(&state, &session_id)?;
    let _ = pty
        .cmd_tx
        .send(crate::types::TerminalCommand::Resize { cols, rows })
        .await;
    Ok(())
}

#[tauri::command]
pub async fn terminal_session_reset(
    session_id: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let pty = ensure_pty(&state, &session_id)?;
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    pty.cmd_tx
        .send(crate::types::TerminalCommand::Reset { response_tx })
        .await
        .map_err(|e| e.to_string())?;
    await_response(response_rx).await
}

/// 面板 session 选择（按对话 PTY 模型下无需后端选择，恒为 no-op）。
#[tauri::command]
pub async fn terminal_panel_set_session(
    _session_id: Option<String>,
    _state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    Ok(())
}

/// 前端 xterm.js 回传当前屏幕快照。
///
/// 前端在终端内容变化时，把 xterm.js buffer.active 的可见区域序列化成文本回传。
/// 后端缓存到对应 session 的 TerminalManager，`handle_exec_interactive` 读取此快照
/// 返回给 Agent——这样 Agent 看到的是与用户一致的屏幕内容（含 vim/nano 全屏界面），
/// 而非后端单行 processor 无法重建的碎片。
#[tauri::command]
pub async fn terminal_session_update_screen(
    session_id: String,
    snapshot: String,
    state: State<'_, TerminalPluginState>,
) -> Result<(), String> {
    let pty = ensure_pty(&state, &session_id)?;
    pty.manager.update_screen_snapshot(snapshot);
    Ok(())
}
