use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

// Marker 前缀常量
pub(crate) const MARKER_START: &str = "__TIANGONG_START_";
pub(crate) const MARKER_END: &str = "__TIANGONG_END_";
pub(crate) const MARKER_CWD: &str = "__TIANGONG_CWD_";
pub(crate) const MARKER_RC: &str = "__TIANGONG_RC_";

/// 判断文本是否包含任何内部 marker
pub(crate) fn contains_marker(text: &str) -> bool {
    text.contains(MARKER_START)
        || text.contains(MARKER_END)
        || text.contains(MARKER_CWD)
        || text.contains(MARKER_RC)
}

/// PTY 进程状态（writer/reader/master/child）
pub(crate) struct PtyState {
    pub writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    pub reader: Arc<Mutex<Box<dyn std::io::Read + Send>>>,
    #[allow(dead_code)]
    pub master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    #[allow(dead_code)]
    pub child: Box<dyn portable_pty::Child + Send>,
}

/// marker 命令执行结果收集
pub(crate) struct CollectResult {
    pub cwd: String,
    pub exit_code: i32,
}

/// 终端内部命令
pub enum TerminalCommand {
    Exec {
        command: String,
        timeout_secs: Option<u64>,
        response_tx: oneshot::Sender<TerminalExecResponse>,
    },
    /// 交互式命令执行（vi/nano/REPL 等），不使用 marker 协议，直接 CR 提交并等待初始输出
    ExecInteractive {
        command: String,
        wait_secs: u64,
        response_tx: oneshot::Sender<TerminalExecResponse>,
    },
    RecentOutput {
        lines: usize,
        response_tx: oneshot::Sender<String>,
    },
    CurrentCwd {
        response_tx: oneshot::Sender<Option<String>>,
    },
    SendInput {
        input: String,
        source: crate::collaboration::InputSource,
        response_tx: oneshot::Sender<()>,
    },
    /// 交互式输入：向已进入交互态的终端发送按键/文本，等待屏幕变化后返回快照
    SendInteractive {
        input: String,
        wait_secs: u64,
        response_tx: oneshot::Sender<TerminalExecResponse>,
    },
    Reset {
        response_tx: oneshot::Sender<()>,
    },
    SetCwd {
        cwd: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
}

/// 终端命令执行响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalExecResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cwd_after: String,
    pub interrupted_by_user: bool,
    pub interactive_mode: bool,
}

impl From<TerminalExecResponse> for tiangong_core::terminal_trait::TerminalExecResult {
    fn from(r: TerminalExecResponse) -> Self {
        Self {
            exit_code: r.exit_code,
            stdout: r.stdout,
            stderr: r.stderr,
            timed_out: r.timed_out,
            cwd_after: r.cwd_after,
            interrupted_by_user: r.interrupted_by_user,
            interactive_mode: r.interactive_mode,
        }
    }
}

/// 终端输出事件（推送到前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalOutputEvent {
    /// 会话 ID
    pub session_id: String,
    /// 输出文本
    pub text: String,
    /// 是否为用户输入的回显
    pub is_echo: bool,
}

/// 终端会话状态（前端查询）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSessionInfo {
    pub session_id: String,
    pub cwd: String,
    pub shell: String,
    pub alive: bool,
}

/// 终端会话状态摘要（前端轮询用），含协作状态 phase
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSessionStatus {
    pub session_id: String,
    pub alive: bool,
    pub cwd: String,
    pub shell: String,
    /// 协作阶段：Idle / Running / Interactive / UserActive
    pub phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalTabInfo {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub alive: bool,
    pub cwd: String,
    pub shell: String,
    pub phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalTabListResponse {
    pub tabs: Vec<TerminalTabInfo>,
    pub active_tab_id: Option<String>,
}
