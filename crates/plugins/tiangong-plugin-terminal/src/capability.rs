//! 终端会话能力抽象。
//!
//! 原 `tiangong-core::terminal_trait`，随能力下沉重构迁入本插件（#225）。
//! `TerminalProvider` 的唯一定义方、实现方、调用方都在本插件内：
//! - 定义：本模块
//! - 实现：[`crate::session_pty::SessionAwareTerminalProvider`]
//! - 调用：[`crate::handler::TerminalToolOverride`]
//!
//! core 不再感知终端能力——终端能力是插件内部状态，由 handler 直接持有 provider 调用，
//! 无需经 RuntimeEngine 中转注入。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::collaboration::TerminalActivityTracker;
use crate::types::TerminalExecResponse;

/// 终端选择结果。
///
/// `terminal_id` 是实际执行命令时使用的路由 id。纯 session id 表示当前默认终端；
/// `session_id:tab_id` 表示指定终端 Tab。
#[derive(Debug, Clone)]
pub struct TerminalSelection {
    pub session_id: String,
    pub tab_id: String,
    pub terminal_id: String,
    pub created_new: bool,
    pub reason: TerminalSelectionReason,
    _reservation: Option<Arc<TerminalSelectionReservation>>,
}

struct TerminalSelectionReservation {
    activity: Arc<TerminalActivityTracker>,
    command_id: String,
}

impl std::fmt::Debug for TerminalSelectionReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalSelectionReservation")
            .field("command_id", &self.command_id)
            .finish_non_exhaustive()
    }
}

impl Drop for TerminalSelectionReservation {
    fn drop(&mut self) {
        self.activity.release_agent_reservation(&self.command_id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSelectionReason {
    ReusedIdle,
    NoAvailableTerminal,
    AllBusy,
}

impl TerminalSelection {
    fn unreserved(session_id: String) -> Self {
        Self {
            session_id: session_id.clone(),
            tab_id: String::new(),
            terminal_id: session_id,
            created_new: false,
            reason: TerminalSelectionReason::ReusedIdle,
            _reservation: None,
        }
    }

    pub(crate) fn reserve(
        session_id: String,
        tab_id: String,
        terminal_id: String,
        created_new: bool,
        reason: TerminalSelectionReason,
        activity: Arc<TerminalActivityTracker>,
    ) -> Option<Self> {
        let command_id = format!("reserved-{}", scru128::new());
        if !activity.try_reserve_agent_command(command_id.clone()) {
            return None;
        }
        Some(Self {
            session_id,
            tab_id,
            terminal_id,
            created_new,
            reason,
            _reservation: Some(Arc::new(TerminalSelectionReservation {
                activity,
                command_id,
            })),
        })
    }

    pub fn feedback_text(&self) -> String {
        let reason = match self.reason {
            TerminalSelectionReason::ReusedIdle => "复用空闲终端",
            TerminalSelectionReason::NoAvailableTerminal => "当前会话没有可用终端",
            TerminalSelectionReason::AllBusy => "当前会话已有终端都在忙",
        };
        if self.created_new {
            format!(
                "；本次在新终端 {} 中执行，原因：{}，没有写入旧终端",
                self.terminal_id, reason
            )
        } else {
            format!("；本次使用终端 {} 执行，原因：{}", self.terminal_id, reason)
        }
    }
}

/// 终端会话能力抽象。
///
/// 所有方法都显式接收 `session_id`，按对话路由到对应对话的 PTY。
/// 这样避免了全局 mutable state 在并发工具调用时的竞态。
pub trait TerminalProvider: Send + Sync + 'static {
    /// 为一次命令执行选择终端。默认返回当前 session，保持非多 Tab provider 兼容。
    fn select_for_command(
        &self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<TerminalSelection>> + Send>> {
        let session_id = session_id.to_string();
        Box::pin(async move { Some(TerminalSelection::unreserved(session_id)) })
    }

    /// 在指定对话的终端会话中执行 shell 脚本（run_shell 用），返回执行结果。
    /// 返回 None 表示终端会话不可用，调用方应回退到独立进程模式。
    fn exec(
        &self,
        session_id: &str,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = Option<TerminalExecResult>> + Send>>;

    /// 在指定对话的终端会话中执行原始命令（run_command 用）。
    fn exec_command(
        &self,
        session_id: &str,
        cmd: &str,
        args: &[String],
        timeout_secs: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = Option<TerminalExecResult>> + Send>>;

    /// 在指定对话的终端会话中以交互模式启动命令。
    ///
    /// 与 `exec` 不同：不使用 marker 协议包裹命令，直接以 CR 提交命令行；
    /// 等待 `wait_secs` 秒后收集初始输出并返回 `interactive_mode: true`，
    /// 终端协作状态进入 `AgentInteractive`，前台交互进程继续运行。
    /// 后续输入由用户在终端面板手动操作，或由 Agent 通过 `send_input` 驱动。
    fn exec_interactive(
        &self,
        session_id: &str,
        command: &str,
        wait_secs: u64,
    ) -> Pin<Box<dyn Future<Output = Option<TerminalExecResult>> + Send>>;

    /// 在指定对话的终端会话中以交互模式启动原始命令（cmd + args）。
    fn exec_command_interactive(
        &self,
        session_id: &str,
        cmd: &str,
        args: &[String],
        wait_secs: u64,
    ) -> Pin<Box<dyn Future<Output = Option<TerminalExecResult>> + Send>>;

    /// 获取指定对话终端最近的输出（环形缓冲区）。
    fn recent_output(
        &self,
        session_id: &str,
        lines: usize,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>>;

    /// 获取指定对话终端的当前工作目录。
    fn current_cwd(&self, session_id: &str)
        -> Pin<Box<dyn Future<Output = Option<String>> + Send>>;

    /// 向指定对话终端的 stdin 发送输入。
    fn send_input(
        &self,
        session_id: &str,
        input: &str,
    ) -> Pin<Box<dyn Future<Output = Option<()>> + Send>>;

    /// 向已进入交互态的终端发送输入（按键/文本），并等待屏幕变化后返回新快照。
    ///
    /// 这是 Agent 持续操作交互程序的核心能力：每发一次按键，
    /// 自动等待屏幕渲染稳定，返回当前可见内容。Agent 据此观察程序对输入的反应，
    /// 形成持续的"输入→观察→输入"闭环。
    /// 与 `exec_interactive`（首次启动交互程序）配套使用。
    fn send_interactive(
        &self,
        session_id: &str,
        input: &str,
        wait_secs: u64,
    ) -> Pin<Box<dyn Future<Output = Option<TerminalExecResult>> + Send>>;

    /// 重置指定对话的终端会话（清理状态，重新初始化）。
    fn reset(&self, session_id: &str) -> Pin<Box<dyn Future<Output = Option<()>> + Send>>;
}

/// 终端命令执行结果。
///
/// 与 [`crate::types::TerminalExecResponse`] 字段一致，后者是 Tauri command 层的
/// 序列化形态；两者之间可直接转换。
#[derive(Debug, Clone)]
pub struct TerminalExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// 终端或命令提交链路失败，而不是 shell 命令自身返回失败。
    pub terminal_error: bool,
    pub timed_out: bool,
    pub cwd_after: String,
    /// 命令被用户中断（如 Ctrl+C）
    pub interrupted_by_user: bool,
    /// 命令进入交互模式（如 vi、python REPL），前台进程仍在运行
    pub interactive_mode: bool,
}

impl From<TerminalExecResponse> for TerminalExecResult {
    fn from(r: TerminalExecResponse) -> Self {
        Self {
            exit_code: r.exit_code,
            stdout: r.stdout,
            stderr: r.stderr,
            terminal_error: r.terminal_error,
            timed_out: r.timed_out,
            cwd_after: r.cwd_after,
            interrupted_by_user: r.interrupted_by_user,
            interactive_mode: r.interactive_mode,
        }
    }
}
