//! 终端能力抽象。
//!
//! GUI 模式下由 `tiangong-plugin-terminal` 实现，CLI/Server 模式下为 None（命令退回子进程）。
//! 设计目标见 `docs/rfc/0014-terminal-integration.md`：每会话单 PTY，所有命令统一走 PTY。

use std::future::Future;
use std::pin::Pin;

/// 终端执行结果（纯数据，无 tokio 依赖）。
#[derive(Debug, Clone, Default)]
pub struct TerminalExecResult {
    /// 命令是否成功（exit_code == 0 且未超时）
    pub ok: bool,
    /// 标准输出（含 stderr，PTY 不分通道）
    pub stdout: String,
    /// 错误提示，给 agent 看的精简描述
    pub stderr: String,
    /// 退出码；超时/中断为 -1
    pub exit_code: i32,
    /// 命令执行后的 cwd（若 PTY 能感知）
    pub cwd_after: String,
    /// 是否超时
    pub timed_out: bool,
    /// 是否被用户中断
    pub interrupted_by_user: bool,
    /// 是否进入交互模式（前台交互程序运行中）
    pub interactive_mode: bool,
}

/// 终端会话当前快照（用于绿点指示器、UI 状态同步）。
#[derive(Debug, Clone, Default)]
pub struct TerminalStatus {
    /// PTY 是否存活
    pub alive: bool,
    /// 当前是否处于交互模式（有前台交互程序）
    pub interactive: bool,
    /// 是否有非交互命令正在执行
    pub running: bool,
}

/// 终端能力 trait。
///
/// 实现方需要保证：
/// - `pty_start` / `pty_send` / `pty_read` / `pty_stop` 操作同一个会话 PTY
/// - `run_command_via_pty` 追加 `__END__<exit>` marker 并解析
/// - 多会话间 PTY 互相隔离，按 session_id 路由
pub trait TerminalProvider: Send + Sync + 'static {
    /// 启动一个交互式程序到当前会话 PTY。
    /// 返回初始屏幕内容，状态转 Interactive。
    fn pty_start(
        &self,
        session_id: &str,
        command: &str,
        cwd: Option<&str>,
        wait_ms: u64,
    ) -> Pin<Box<dyn Future<Output = TerminalExecResult> + Send>>;

    /// 向当前会话 PTY 发送键盘输入。
    /// 实现方需要先解析字面转义、把 LF 转 CR 再写入。
    fn pty_send(
        &self,
        session_id: &str,
        input: &str,
        wait_ms: u64,
        lines: usize,
    ) -> Pin<Box<dyn Future<Output = Option<TerminalExecResult>> + Send>>;

    /// 读取当前会话 PTY 的最近 N 行屏幕快照（不发送输入）。
    fn pty_read(
        &self,
        session_id: &str,
        lines: usize,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send>>;

    /// 停止当前会话 PTY 上的前台交互程序。
    /// 顺序：Esc → Ctrl+C → 200ms → 强杀子进程重启 shell。
    fn pty_stop(&self, session_id: &str) -> Pin<Box<dyn Future<Output = Option<()>> + Send>>;

    /// 通过 PTY 执行非交互命令（追加 end marker 捕获退出码）。
    /// `command` 应当是被 shell 解析的完整脚本字符串。
    fn run_command_via_pty(
        &self,
        session_id: &str,
        command: &str,
        cwd: Option<&str>,
        timeout_secs: u64,
    ) -> Pin<Box<dyn Future<Output = TerminalExecResult> + Send>>;

    /// 查询某会话 PTY 的状态（用于前端绿点）。
    fn status(&self, session_id: &str) -> Pin<Box<dyn Future<Output = TerminalStatus> + Send>>;

    /// 销毁指定会话的 PTY（会话删除时调用）。
    fn destroy(&self, _session_id: &str) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}
