use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub(crate) type SharedPtyWriter = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

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
    pub writer: SharedPtyWriter,
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
        cancellation: Arc<TerminalExecCancellation>,
        completion: TerminalExecCompletion,
    },
    /// 交互式命令执行，不使用 marker 协议，直接 CR 提交并等待初始输出
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
        response_tx: oneshot::Sender<Result<(), String>>,
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

/// 一次非交互 PTY 命令的取消/完成栅栏。
///
/// 调用 Future 被 drop 时同步请求取消并等待 command loop 确认命令边界闭合，
/// 或完成不可忽略的强制终止；Agent Team 因而只会在真实命令停止后释放文件锁。
#[derive(Default)]
pub struct TerminalExecCancellation {
    requested: AtomicBool,
    finished: Mutex<bool>,
    ready: Condvar,
}

/// 随排队的 Exec 命令移动的完成所有权。
///
/// 即使 command loop 在取出请求前退出、队列 receiver 被 drop，payload 的 Drop
/// 仍会释放调用 Future 正在等待的完成栅栏。
pub struct TerminalExecCompletion {
    cancellation: Arc<TerminalExecCancellation>,
}

impl TerminalExecCompletion {
    pub(crate) fn new(cancellation: Arc<TerminalExecCancellation>) -> Self {
        Self { cancellation }
    }
}

impl Drop for TerminalExecCompletion {
    fn drop(&mut self) {
        self.cancellation.mark_finished();
    }
}

impl TerminalExecCancellation {
    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub(crate) fn request_and_wait(&self) {
        self.requested.store(true, Ordering::Release);
        let mut finished = self
            .finished
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while !*finished {
            finished = self
                .ready
                .wait(finished)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }

    pub(crate) fn mark_finished(&self) {
        let mut finished = self
            .finished
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *finished = true;
        self.ready.notify_all();
    }
}

/// 终端命令执行响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalExecResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// 命令协议或 PTY 本身失败；不是用户命令返回的普通非零退出码。
    pub terminal_error: bool,
    pub timed_out: bool,
    pub cwd_after: String,
    pub interrupted_by_user: bool,
    pub interactive_mode: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalTabUpdatedEvent {
    pub session_id: String,
    pub active_tab_id: Option<String>,
    pub source: String,
}
