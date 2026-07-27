use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::command_protocol;
use crate::output_processor;
use crate::types::{contains_marker, PtyState, TerminalCommand};
use crate::util::shell_quote;

const DEFAULT_BUFFER_LINES: usize = 5000;

pub struct TerminalManager {
    pub(crate) state: Arc<Mutex<TerminalState>>,
    /// 系统 PTY 输出日志器（仅系统 PTY 有；面板 PTY 为 None）。用于持久化历史与重置时清空。
    pub(crate) logger: Option<Arc<crate::output_processor::OutputLogger>>,
    /// 创建 PTY 时快照的插件贡献环境变量（由 core 经 `set_exec_env` 汇总回注）。
    ///
    /// PTY 是长期复用的交互式 shell，env 在创建时快照注入（不 clear、不 allowlist，
    /// 只在继承的主进程环境之上追加）。Reset / PTY 恢复时复用同一份快照重建 shell，
    /// 保持会话内环境一致。配置变更后新建的 PTY 才会用最新 env（快照语义，
    /// 与 `default_cwd` 一致）。
    pub(crate) pty_env: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
}

pub(crate) struct TerminalState {
    pub session_id: String,
    pub cwd: String,
    pub shell: String,
    pub alive: bool,
    pub output_buffer: VecDeque<String>,
    pub buffer_limit: usize,
    /// 上次输出读取位置（用于增量返回）
    pub last_read_line: usize,
    /// 累计总行数（用于 start marker 定位，不受环形缓冲区 pop_front 影响）
    pub total_lines_pushed: usize,
    /// 当前尚未换行的屏幕行（如 Password:、Proceed? [Y/n] 等提示）
    pub current_line: String,
    /// 前端 xterm.js 回传的屏幕快照（终端可见区域的文本序列化）。
    ///
    /// 后端的 `TerminalLineProcessor` 是单行模型，无法重建全屏 TUI 界面
    ///（光标在屏幕各处定位、多行同时存在）。前端 xterm.js 维护了完整的二维屏幕缓冲区，
    ///（正是它渲染了用户看到的终端画面），由前端在内容变化时序列化回传。
    /// `handle_exec_interactive` 返回此快照，让 Agent 看到与用户一致的屏幕内容。
    pub screen_snapshot: Option<String>,
    /// 屏幕快照更新计数：每次前端回传新快照时递增。
    /// `handle_exec_interactive` 用它检测"屏幕是否发生变化"（替代 output_buffer 计数）。
    pub screen_updates: u64,
}

impl TerminalManager {
    pub fn new(session_id: String, cwd: String) -> Self {
        Self::with_pty_env(session_id, cwd, std::collections::BTreeMap::new())
    }

    /// 用指定的插件贡献 env 快照构造 manager（PTY 创建时注入）。
    pub(crate) fn with_pty_env(
        session_id: String,
        cwd: String,
        pty_env: std::collections::BTreeMap<String, String>,
    ) -> Self {
        let shell = default_shell();
        Self {
            state: Arc::new(Mutex::new(TerminalState {
                session_id,
                cwd,
                shell,
                alive: false,
                output_buffer: VecDeque::with_capacity(DEFAULT_BUFFER_LINES),
                buffer_limit: DEFAULT_BUFFER_LINES,
                last_read_line: 0,
                total_lines_pushed: 0,
                current_line: String::new(),
                screen_snapshot: None,
                screen_updates: 0,
            })),
            logger: None,
            pty_env: Arc::new(Mutex::new(pty_env)),
        }
    }

    /// 设置系统 PTY 输出日志器（启动时回填历史后调用）
    pub(crate) fn set_logger(&mut self, logger: Arc<crate::output_processor::OutputLogger>) {
        self.logger = Some(logger);
    }

    /// 清空输出日志（用户主动重置终端时调用），无日志器时为空操作
    pub(crate) fn clear_log(&self) {
        if let Some(ref logger) = self.logger {
            logger.clear();
        }
    }

    #[allow(private_interfaces)]
    pub(crate) fn clone_state(&self) -> Arc<Mutex<TerminalState>> {
        self.state.clone()
    }

    pub fn session_id(&self) -> String {
        self.state.lock().unwrap().session_id.clone()
    }

    /// 更新前端 xterm.js 回传的屏幕快照（前端内容变化时调用）。
    pub(crate) fn update_screen_snapshot(&self, snapshot: String) {
        let mut state = self.state.lock().unwrap();
        state.screen_snapshot = Some(snapshot);
        state.screen_updates = state.screen_updates.saturating_add(1);
    }

    /// 获取当前屏幕快照（前端回传的可见区域文本）。
    pub fn screen_snapshot(&self) -> Option<String> {
        self.state.lock().unwrap().screen_snapshot.clone()
    }

    /// 获取屏幕快照更新计数（用于检测变化）。
    pub fn screen_updates(&self) -> u64 {
        self.state.lock().unwrap().screen_updates
    }

    /// 获取累计输出行数（程序向终端写数据的可靠信号，用于检测交互程序响应）。
    pub fn total_lines_pushed(&self) -> usize {
        self.state.lock().unwrap().total_lines_pushed
    }

    pub fn cwd(&self) -> String {
        self.state.lock().unwrap().cwd.clone()
    }

    pub fn shell(&self) -> String {
        self.state.lock().unwrap().shell.clone()
    }

    pub fn is_alive(&self) -> bool {
        self.state.lock().unwrap().alive
    }

    pub fn set_alive(&self, alive: bool) {
        self.state.lock().unwrap().alive = alive;
    }

    /// 启动 PTY 并在成功时启动输出读取线程，返回 PtyState
    pub(crate) fn start_and_spawn_reader(
        &self,
        session_id: &str,
        cwd: &str,
        app: tauri::AppHandle,
    ) -> Option<PtyState> {
        let shell = self.shell();
        let pty_env = self.pty_env.lock().map(|g| g.clone()).unwrap_or_default();
        let ps = start_pty(session_id, cwd, &shell, &pty_env)
            .inspect_err(|e| {
                error!(session_id, error = %e, "PTY 进程启动失败");
            })
            .ok()?;
        self.set_alive(true);
        output_processor::spawn_output_reader(
            ps.reader.clone(),
            self.state.clone(),
            app,
            session_id.to_string(),
            self.logger.clone(),
        );
        info!(session_id, "PTY 进程已启动");
        Some(ps)
    }

    /// 获取最近的 N 行输出
    pub fn recent_output(&self, lines: usize) -> String {
        let state = self.state.lock().unwrap();
        if lines == 0 {
            return String::new();
        }

        let current_line = state.current_line.trim_end().to_string();
        let include_current = !current_line.trim().is_empty() && !contains_marker(&current_line);
        let completed_limit = if include_current {
            lines.saturating_sub(1)
        } else {
            lines
        };

        let mut output = state
            .output_buffer
            .iter()
            .rev()
            .filter(|line| !contains_marker(line))
            .take(completed_limit)
            .cloned()
            .collect::<Vec<_>>();
        output.reverse();
        if include_current {
            output.push(current_line);
        }
        output.join("\n")
    }

    /// 获取自上次读取以来的增量输出，并重置标记
    pub fn incremental_output(&self) -> String {
        let mut state = self.state.lock().unwrap();
        let total = state.output_buffer.len();
        if state.last_read_line >= total {
            state.last_read_line = total;
            return String::new();
        }
        let output = state
            .output_buffer
            .iter()
            .skip(state.last_read_line)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        state.last_read_line = total;
        output
    }
}

/// 向环形缓冲区追加一行输出
pub(crate) fn push_output(state: &mut TerminalState, line: String) {
    if state.output_buffer.len() >= state.buffer_limit {
        state.output_buffer.pop_front();
        if state.last_read_line > 0 {
            state.last_read_line -= 1;
        }
    }
    state.output_buffer.push_back(line);
    state.total_lines_pushed += 1;
}

/// 命令处理循环（PTY 已在 setup 阶段同步启动）
pub(crate) async fn spawn_command_loop(
    mut rx: mpsc::Receiver<TerminalCommand>,
    manager: Arc<TerminalManager>,
    app: tauri::AppHandle,
    mut pty_state: Option<PtyState>,
    activity: Option<Arc<crate::collaboration::TerminalActivityTracker>>,
) {
    let session_id = manager.session_id();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            TerminalCommand::Exec {
                command,
                timeout_secs,
                response_tx,
                cancellation,
                completion,
            } => {
                command_protocol::handle_exec(
                    &manager,
                    &mut pty_state,
                    &app,
                    &command,
                    timeout_secs,
                    response_tx,
                    cancellation,
                    completion,
                    activity.as_ref(),
                )
                .await;
            }
            TerminalCommand::ExecInteractive {
                command,
                wait_secs,
                response_tx,
            } => {
                command_protocol::handle_exec_interactive(
                    &manager,
                    &mut pty_state,
                    &app,
                    &command,
                    wait_secs,
                    response_tx,
                    activity.as_ref(),
                )
                .await;
            }
            TerminalCommand::RecentOutput { lines, response_tx } => {
                let output = manager.recent_output(lines);
                let _ = response_tx.send(output);
            }
            TerminalCommand::CurrentCwd { response_tx } => {
                let cwd = manager.cwd();
                _ = response_tx.send(Some(cwd));
            }
            TerminalCommand::SendInput {
                input,
                source,
                response_tx,
            } => {
                let mut write_ok = false;
                if let Some(ref ps) = pty_state {
                    match ps.writer.lock() {
                        Ok(mut writer) => {
                            write_ok = writer
                                .write_all(input.as_bytes())
                                .and_then(|_| writer.flush())
                                .is_ok();
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "终端 writer 锁中毒，输入未发送");
                        }
                    }
                } else {
                    tracing::warn!("PTY 未启动，terminal_input 输入被丢弃");
                }
                if !write_ok {
                    tracing::warn!(
                        input_len = input.len(),
                        "terminal_input 写入 PTY 失败（writer 返回错误或 PTY 已关闭）"
                    );
                }
                if source == crate::collaboration::InputSource::User {
                    if let Some(ref tracker) = activity {
                        tracker.record_user_input();
                    }
                }
                let _ = response_tx.send(());
            }
            TerminalCommand::SendInteractive {
                input,
                wait_secs,
                response_tx,
            } => {
                command_protocol::handle_send_interactive(
                    &manager,
                    &mut pty_state,
                    &app,
                    &input,
                    wait_secs,
                    response_tx,
                    activity.as_ref(),
                )
                .await;
            }
            TerminalCommand::SetCwd { cwd } => {
                {
                    let mut state = manager.state.lock().unwrap();
                    state.cwd = cwd.clone();
                }
                if let Some(ref ps) = pty_state {
                    if let Ok(mut writer) = ps.writer.lock() {
                        let quoted = shell_quote(&cwd);
                        // 用 `\r`（CR）而非 `\n`（LF）：PTY 线路规程只识别 CR 作为回车提交，
                        // 发 LF 在 zsh ZLE 等场景下 cd 命令不会执行
                        let _ = writer.write_all(format!("cd {}\r", quoted).as_bytes());
                        let _ = writer.flush();
                    }
                }
            }
            TerminalCommand::Resize { cols, rows } => {
                if let Some(ref ps) = pty_state {
                    if let Ok(master) = ps.master.lock() {
                        let _ = master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
            }
            TerminalCommand::Reset { response_tx } => {
                // 记录重置前的终端状态，便于排查 agent 是否过早放弃交互
                {
                    let state = manager.state.lock().unwrap();
                    let last_lines: Vec<String> =
                        state.output_buffer.iter().rev().take(5).cloned().collect();
                    let current = state.current_line.clone();
                    let busy = activity
                        .as_ref()
                        .map(|t| format!("{:?}", t.busy_state()))
                        .unwrap_or_else(|| "no-tracker".to_string());
                    tracing::warn!(
                        session_id = %session_id,
                        busy_state = %busy,
                        last_lines = ?last_lines,
                        current_line = %current,
                        "terminal_reset 被调用：丢弃当前 PTY 状态并重启 shell，可能导致交互程序中的未保存数据丢失"
                    );
                }

                // 清理旧 PTY
                if let Some(ps) = pty_state.take() {
                    drop(ps);
                }
                // 重启 PTY
                let cwd = manager.cwd();
                let shell = manager.shell();
                {
                    let mut state = manager.state.lock().unwrap();
                    state.output_buffer.clear();
                    state.last_read_line = 0;
                    state.current_line.clear();
                }
                // 用户主动重置视为刷新，清空持久化日志
                manager.clear_log();
                let pty_env = manager
                    .pty_env
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                match start_pty(&session_id, &cwd, &shell, &pty_env) {
                    Ok(new_ps) => {
                        {
                            let mut state = manager.state.lock().unwrap();
                            state.alive = true;
                        }
                        let state = manager.clone_state();
                        let app_handle = app.clone();
                        let sid = session_id.clone();
                        output_processor::spawn_output_reader(
                            new_ps.reader.clone(),
                            state,
                            app_handle,
                            sid,
                            manager.logger.clone(),
                        );
                        pty_state = Some(new_ps);
                        info!(session_id = %session_id, "PTY 进程已重置");
                    }
                    Err(e) => {
                        error!(error = %e, "PTY 重置失败");
                    }
                }
                let _ = response_tx.send(());
            }
        }
    }

    // 清理 PTY 进程
    if let Some(ps) = pty_state.take() {
        drop(ps);
    }
    {
        let mut state = manager.state.lock().unwrap();
        state.alive = false;
    }
    info!(session_id = %session_id, "终端命令处理器退出");
}

/// 解析默认 shell 可执行文件路径。
///
/// 与 `tiangong_toolkit::derive_shell_exec_args` 的跨平台约定保持一致：
///
/// - **非 Windows**：读取 `SHELL` 环境变量，缺失时 fallback 到 `/bin/bash`。
/// - **Windows**：`SHELL` 是 Unix 概念，Windows 几乎从不设置，且即使设置也通常指向
///   Git Bash 之类的非标准 shell。Windows 默认走 `powershell.exe`（通过 PATH 解析）。
///   旧版 `SHELL` fallback 到 `/bin/bash` 在 Windows 上不存在该路径，会导致
///   `spawn_command` 失败、终端面板提示"PTY 启动失败"（见 issue #151）。
fn default_shell() -> String {
    if cfg!(target_os = "windows") {
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}

/// 推导以「登录 shell」方式启动所需的命令行参数。
///
/// `shell` 可能是绝对路径（`/bin/zsh`、`/usr/local/bin/bash`）或裸名（`bash`），
/// 取 basename 判定。各 shell 的登录启动参数：
/// - **bash / zsh**：`--login`。会读 `/etc/profile`、`~/.zprofile`/`~/.bash_profile`；
///   PTY 是交互式 TTY，zsh 还会读 `~/.zshrc`。Homebrew 默认把
///   `eval "$(brew shellenv)"` 写进 `~/.zprofile`，恰好在此被加载，
///   从而注入 `/opt/homebrew/bin` 等 PATH。
/// - **sh**：`-l`（POSIX 登录 shell 约定）。
/// - **Windows / powershell / 未知 shell**：不传登录参数（无等价概念或语义不明）。
///
/// 返回静态字符串切片以直接喂给 `CommandBuilder::arg`。
fn login_shell_args(shell: &str) -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        return Vec::new();
    }
    let basename = std::path::Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell);
    match basename {
        "bash" | "zsh" => vec!["--login"],
        "sh" => vec!["-l"],
        _ => Vec::new(),
    }
}

pub(crate) fn start_pty(
    session_id: &str,
    cwd: &str,
    shell: &str,
    extra_env: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<PtyState> {
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow::anyhow!("PTY 创建失败: {}", e))?;

    let mut cmd = CommandBuilder::new(shell);
    // 以登录 shell 方式启动（对齐 Terminal.app / iTerm2 / VSCode 的默认行为）：
    // 让 shell source /etc/profile → /etc/paths、~/.zprofile/~/.bash_profile，
    // 从而拿到用户真实 PATH（如 Homebrew 的 /opt/homebrew/bin），避免 GUI 主进程
    // 继承的残缺 PATH 导致 gh 等命令 "command not found"。见 issue #151。
    for arg in login_shell_args(shell) {
        cmd.arg(arg);
    }
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    // 追加各插件贡献的环境变量（MCP/skill 等注入的 token、.env.local 等）。
    // PTY 是交互式 shell，继承主进程完整环境，此处只追加、不 clear、不 allowlist。
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    tracing::debug!(session_id, cwd, shell, "正在启动 PTY 子进程");

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow::anyhow!("PTY 启动 shell 失败: {}", e))?;

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| anyhow::anyhow!("PTY reader clone 失败: {}", e))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| anyhow::anyhow!("PTY writer 获取失败: {}", e))?;

    Ok(PtyState {
        writer: Arc::new(Mutex::new(writer)),
        reader: Arc::new(Mutex::new(reader)),
        master: Arc::new(Mutex::new(pair.master)),
        child,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_buffer_ring() {
        let mut state = TerminalState {
            session_id: "test".to_string(),
            cwd: "/tmp".to_string(),
            shell: "/bin/bash".to_string(),
            alive: false,
            output_buffer: VecDeque::with_capacity(5),
            buffer_limit: 5,
            last_read_line: 0,
            total_lines_pushed: 0,
            current_line: String::new(),
            screen_snapshot: None,
            screen_updates: 0,
        };

        for i in 0..8 {
            push_output(&mut state, format!("line {}", i));
        }

        assert_eq!(state.output_buffer.len(), 5);
        assert_eq!(state.output_buffer[0], "line 3");
        assert_eq!(state.output_buffer[4], "line 7");
    }

    #[test]
    fn test_incremental_output() {
        let manager = TerminalManager::new("test".to_string(), "/tmp".to_string());

        // 添加一些输出
        {
            let mut state = manager.state.lock().unwrap();
            push_output(&mut state, "line 1".to_string());
            push_output(&mut state, "line 2".to_string());
        }

        let inc = manager.incremental_output();
        assert_eq!(inc, "line 1\nline 2");

        // 添加更多输出
        {
            let mut state = manager.state.lock().unwrap();
            push_output(&mut state, "line 3".to_string());
        }

        let inc2 = manager.incremental_output();
        assert_eq!(inc2, "line 3");

        // 没有新输出
        let inc3 = manager.incremental_output();
        assert!(inc3.is_empty());
    }

    // login_shell_args 仅在非 Windows 上产出登录参数，相关断言限定平台编译，
    // 避免 Windows 下因函数固定返回空而必然失败。
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_login_shell_args() {
        // macOS 常见绝对路径 → basename 判定
        assert_eq!(login_shell_args("/bin/zsh"), vec!["--login"]);
        assert_eq!(login_shell_args("/bin/bash"), vec!["--login"]);
        // Homebrew 安装的 shell 路径
        assert_eq!(login_shell_args("/opt/homebrew/bin/zsh"), vec!["--login"]);
        assert_eq!(login_shell_args("/usr/local/bin/bash"), vec!["--login"]);
        // 裸名
        assert_eq!(login_shell_args("zsh"), vec!["--login"]);
        assert_eq!(login_shell_args("bash"), vec!["--login"]);
        // POSIX sh
        assert_eq!(login_shell_args("/bin/sh"), vec!["-l"]);
        assert_eq!(login_shell_args("sh"), vec!["-l"]);
        // 未知 shell：不传登录参数（保持原行为，避免错误语义）
        assert!(login_shell_args("/usr/bin/fish").is_empty());
        assert!(login_shell_args("fish").is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_login_shell_args_on_windows() {
        // Windows 下无论什么 shell 都不应传登录参数（无 -l/--login 概念）。
        assert!(login_shell_args("powershell.exe").is_empty());
        assert!(login_shell_args("bash").is_empty());
    }
}
