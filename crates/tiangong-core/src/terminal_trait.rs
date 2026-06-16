use std::future::Future;
use std::pin::Pin;

/// 终端会话能力抽象。
///
/// GUI 模式下由 tiangong-plugin-terminal 实现，CLI/Server 模式下为 None（回退到独立进程）。
///
/// 所有方法都显式接收 `session_id`，按对话路由到对应对话的 PTY。
/// 这样避免了全局 mutable state 在并发工具调用时的竞态。
pub trait TerminalProvider: Send + Sync + 'static {
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

    /// 重置指定对话的终端会话（清理状态，重新初始化）。
    fn reset(&self, session_id: &str) -> Pin<Box<dyn Future<Output = Option<()>> + Send>>;
}

/// 终端命令执行结果
#[derive(Debug, Clone)]
pub struct TerminalExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cwd_after: String,
    /// 命令被用户中断（如 Ctrl+C）
    pub interrupted_by_user: bool,
    /// 命令进入交互模式（如 vi、python REPL），前台进程仍在运行
    pub interactive_mode: bool,
}
