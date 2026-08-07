use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::command_protocol;
use crate::output_processor;
use crate::types::{contains_marker, PtyState, SharedPtyWriter, TerminalCommand};
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
    /// 用户已关闭该终端。与 PTY 意外退出分开记录，避免关闭后被执行路径自动恢复。
    pub closed: bool,
    /// 当前 PTY 实例代次。重置或恢复会递增，防止旧 reader 退出后误标记新 PTY 死亡。
    pub pty_generation: u64,
    /// 当前 PTY writer。用户键盘输入通过这里直接写入，不受命令队列阻塞。
    pub writer: Option<SharedPtyWriter>,
    pub output_buffer: VecDeque<String>,
    pub buffer_limit: usize,
    /// 上次输出读取位置（用于增量返回）
    pub last_read_line: usize,
    /// 累计总行数（用于 start marker 定位，不受环形缓冲区 pop_front 影响）
    pub total_lines_pushed: usize,
    /// 最近一次成功注入 Agent 的输出游标；注入失败时不推进。
    pub agent_injection_cursor: usize,
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
                closed: false,
                pty_generation: 0,
                writer: None,
                output_buffer: VecDeque::with_capacity(DEFAULT_BUFFER_LINES),
                buffer_limit: DEFAULT_BUFFER_LINES,
                last_read_line: 0,
                total_lines_pushed: 0,
                agent_injection_cursor: 0,
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

    pub(crate) fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    /// 标记终端已被用户关闭，并立即切断后续写入。
    pub(crate) fn close(&self) {
        let writer = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.closed = true;
            state.alive = false;
            state.writer.take()
        };
        // 与已经取得 writer 引用的并发写入同步；close 返回后不会再有输入落到 PTY。
        if let Some(writer) = writer {
            let _writer_guard = writer.lock().unwrap_or_else(|poison| poison.into_inner());
        }
    }

    /// 激活一个新 PTY writer 并返回对应代次。
    pub(crate) fn activate_pty(&self, writer: SharedPtyWriter) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.pty_generation = state.pty_generation.wrapping_add(1);
        state.alive = true;
        state.writer = Some(writer);
        state.pty_generation
    }

    /// 仅当退出的是当前 PTY 代次时标记死亡。
    pub(crate) fn mark_pty_stopped(&self, generation: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.pty_generation != generation {
            return false;
        }
        state.alive = false;
        state.writer = None;
        true
    }

    pub(crate) fn deactivate_pty(&self) {
        let mut state = self.state.lock().unwrap();
        state.alive = false;
        state.writer = None;
    }

    /// 直接写入当前 PTY。用于用户键盘输入，避免被正在等待的 Agent 命令阻塞。
    pub(crate) fn write_input(&self, input: &[u8]) -> Result<(), String> {
        let (generation, writer) = {
            let state = self.state.lock().map_err(|e| e.to_string())?;
            if !state.alive {
                return Err("终端进程已退出".to_string());
            }
            let writer = state
                .writer
                .clone()
                .ok_or_else(|| "终端输入通道不可用".to_string())?;
            (state.pty_generation, writer)
        };

        let write_result = match writer.lock() {
            Ok(mut writer) => {
                let still_active = self
                    .state
                    .lock()
                    .map(|state| state.alive && state.pty_generation == generation)
                    .unwrap_or(false);
                if !still_active {
                    Err("终端进程已退出".to_string())
                } else {
                    writer
                        .write_all(input)
                        .and_then(|_| writer.flush())
                        .map_err(|e| e.to_string())
                }
            }
            Err(e) => Err(format!("终端输入通道锁定失败: {e}")),
        };
        if let Err(error) = write_result {
            self.mark_pty_stopped(generation);
            return Err(error);
        }
        Ok(())
    }

    /// 启动 PTY 并在成功时启动输出读取线程，返回 PtyState
    pub(crate) fn start_and_spawn_reader<R: tauri::Runtime>(
        &self,
        session_id: &str,
        cwd: &str,
        app: tauri::AppHandle<R>,
    ) -> Option<PtyState> {
        let shell = self.shell();
        let pty_env = self.pty_env.lock().map(|g| g.clone()).unwrap_or_default();
        let ps = start_pty(session_id, cwd, &shell, &pty_env)
            .inspect_err(|e| {
                error!(session_id, error = %e, "PTY 进程启动失败");
            })
            .ok()?;
        let generation = self.activate_pty(ps.writer.clone());
        output_processor::spawn_output_reader(
            ps.reader.clone(),
            self.state.clone(),
            app,
            session_id.to_string(),
            self.logger.clone(),
            generation,
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

    /// 读取尚未成功注入 Agent 的完整行，不推进游标。
    ///
    /// 返回 `(输出, 起始游标, 结束游标, 是否截断)`。调用方只有在 terminal_data
    /// 成功入队后才能提交结束游标；快照后新增的输出自然留到下一轮。
    pub fn pending_agent_output(&self, max_lines: usize) -> (String, usize, usize, bool) {
        let state = self.state.lock().unwrap();
        let end = state.total_lines_pushed;
        let oldest = end.saturating_sub(state.output_buffer.len());
        let requested_start = state.agent_injection_cursor.min(end);
        let available_start = requested_start.max(oldest);
        let limited_start = if max_lines == 0 {
            end
        } else {
            available_start.max(end.saturating_sub(max_lines))
        };
        let truncated = limited_start > requested_start;
        let offset = limited_start.saturating_sub(oldest);
        let output = state
            .output_buffer
            .iter()
            .skip(offset)
            .filter(|line| !contains_marker(line))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        (output, limited_start, end, truncated)
    }

    /// 提交已成功入队的 Agent 输出游标；迟到提交不会让游标倒退。
    pub fn commit_agent_injection(&self, end_cursor: usize) {
        let mut state = self.state.lock().unwrap();
        state.agent_injection_cursor = state
            .agent_injection_cursor
            .max(end_cursor.min(state.total_lines_pushed));
    }

    /// Core 新实例接管已有终端时，从当前末尾开始观察，避免把历史输出重新注入。
    pub fn baseline_agent_injection(&self) {
        let mut state = self.state.lock().unwrap();
        state.agent_injection_cursor = state.total_lines_pushed;
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
pub(crate) async fn spawn_command_loop<R: tauri::Runtime>(
    mut rx: mpsc::Receiver<TerminalCommand>,
    manager: Arc<TerminalManager>,
    app: tauri::AppHandle<R>,
    mut pty_state: Option<PtyState>,
    activity: Option<Arc<crate::collaboration::TerminalActivityTracker>>,
) {
    let session_id = manager.session_id();

    while let Some(cmd) = rx.recv().await {
        if manager.is_closed() {
            break;
        }
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
                let result = if pty_state.is_some() {
                    manager.write_input(input.as_bytes())
                } else {
                    Err("PTY 未启动".to_string())
                };
                if let Err(ref error) = result {
                    tracing::warn!(
                        error,
                        input_len = input.len(),
                        "terminal_input 写入 PTY 失败"
                    );
                }
                if result.is_ok() && source == crate::collaboration::InputSource::User {
                    if let Some(ref tracker) = activity {
                        tracker.record_user_input();
                    }
                }
                let _ = response_tx.send(result);
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
                    manager.deactivate_pty();
                    shutdown_pty(ps);
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
                        let generation = manager.activate_pty(new_ps.writer.clone());
                        let state = manager.clone_state();
                        let app_handle = app.clone();
                        let sid = session_id.clone();
                        output_processor::spawn_output_reader(
                            new_ps.reader.clone(),
                            state,
                            app_handle,
                            sid,
                            manager.logger.clone(),
                            generation,
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
        manager.deactivate_pty();
        shutdown_pty(ps);
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

/// 取得 PTY 当前前台进程组。交互 shell 开启 job control 后，当前用户命令通常
/// 位于独立进程组；关闭终端时必须先终止该组，不能只杀登录 shell。
#[cfg(unix)]
pub(crate) fn foreground_process_group(ps: &PtyState) -> Option<libc::pid_t> {
    ps.master
        .lock()
        .ok()
        .and_then(|master| master.process_group_leader())
        .filter(|process_group| *process_group > 0)
}

#[cfg(unix)]
pub(crate) fn force_stop_process_group(process_group: libc::pid_t) -> std::io::Result<()> {
    force_stop_target(-process_group)
}

#[cfg(unix)]
pub(crate) fn force_stop_process(process_id: libc::pid_t) -> std::io::Result<()> {
    force_stop_target(process_id)
}

#[cfg(unix)]
fn force_stop_target(target: libc::pid_t) -> std::io::Result<()> {
    let result = unsafe { libc::kill(target, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

/// 终止前台任务并回收 PTY shell，避免关闭终端后留下孤立命令或未回收进程。
pub(crate) fn shutdown_pty(ps: PtyState) {
    #[cfg(unix)]
    if let Some(process_group) = foreground_process_group(&ps) {
        if let Err(error) = force_stop_process_group(process_group) {
            tracing::warn!(%error, process_group, "终止 PTY 前台进程组失败");
        }
    }

    let PtyState {
        writer,
        reader,
        master,
        mut child,
    } = ps;

    let should_wait = match child.try_wait() {
        Ok(Some(_)) => false,
        Ok(None) => {
            #[cfg(unix)]
            let kill_result = child
                .process_id()
                .map(|pid| force_stop_process(pid as libc::pid_t))
                .unwrap_or_else(|| Err(std::io::Error::other("PTY 子进程 PID 不可用")));
            #[cfg(not(unix))]
            let kill_result = child.kill();
            let kill_succeeded = kill_result.is_ok();
            if let Err(error) = kill_result {
                tracing::warn!(%error, "终止 PTY 子进程失败");
            }
            kill_succeeded
        }
        Err(error) => {
            tracing::warn!(%error, "查询 PTY 子进程状态失败");
            false
        }
    };

    // macOS 上会话首进程收到终止信号后，可能要等 PTY master 的全部句柄关闭
    // 才能完成退出。持有这些句柄调用 wait 会让关闭流程永久卡住。
    drop(writer);
    drop(reader);
    drop(master);

    if should_wait {
        if let Err(error) = child.wait() {
            tracing::warn!(%error, "回收 PTY 子进程失败");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct FailingWriter;

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_output_buffer_ring() {
        let mut state = TerminalState {
            session_id: "test".to_string(),
            cwd: "/tmp".to_string(),
            shell: "/bin/bash".to_string(),
            alive: false,
            closed: false,
            pty_generation: 0,
            writer: None,
            output_buffer: VecDeque::with_capacity(5),
            buffer_limit: 5,
            last_read_line: 0,
            total_lines_pushed: 0,
            agent_injection_cursor: 0,
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
    fn agent_output_cursor_only_advances_after_commit() {
        let manager = TerminalManager::new("test".to_string(), "/tmp".to_string());
        {
            let mut state = manager.state.lock().unwrap();
            push_output(&mut state, "line 1".to_string());
            push_output(&mut state, "line 2".to_string());
        }

        let first = manager.pending_agent_output(40);
        assert_eq!(first, ("line 1\nline 2".to_string(), 0, 2, false));
        assert_eq!(manager.pending_agent_output(40), first);

        manager.commit_agent_injection(first.2);
        assert_eq!(
            manager.pending_agent_output(40),
            (String::new(), 2, 2, false)
        );

        {
            let mut state = manager.state.lock().unwrap();
            push_output(&mut state, "line 3".to_string());
        }
        assert_eq!(
            manager.pending_agent_output(40),
            ("line 3".to_string(), 2, 3, false)
        );
    }

    #[test]
    fn restored_core_baselines_agent_output_at_current_end() {
        let manager = TerminalManager::new("test".to_string(), "/tmp".to_string());
        {
            let mut state = manager.state.lock().unwrap();
            push_output(&mut state, "historical".to_string());
        }

        manager.baseline_agent_injection();
        assert_eq!(
            manager.pending_agent_output(40),
            (String::new(), 1, 1, false)
        );

        {
            let mut state = manager.state.lock().unwrap();
            push_output(&mut state, "new".to_string());
        }
        assert_eq!(
            manager.pending_agent_output(40),
            ("new".to_string(), 1, 2, false)
        );
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

    #[test]
    fn direct_input_uses_current_pty_generation() {
        let manager = TerminalManager::new("test".to_string(), "/tmp".to_string());
        let first_output = Arc::new(Mutex::new(Vec::new()));
        let first_writer: SharedPtyWriter =
            Arc::new(Mutex::new(Box::new(RecordingWriter(first_output.clone()))));
        let first_generation = manager.activate_pty(first_writer);

        manager.write_input(b"first").unwrap();
        assert_eq!(&*first_output.lock().unwrap(), b"first");

        let second_output = Arc::new(Mutex::new(Vec::new()));
        let second_writer: SharedPtyWriter =
            Arc::new(Mutex::new(Box::new(RecordingWriter(second_output.clone()))));
        let second_generation = manager.activate_pty(second_writer);

        assert!(!manager.mark_pty_stopped(first_generation));
        assert!(manager.is_alive());
        manager.write_input(b"second").unwrap();
        assert_eq!(&*second_output.lock().unwrap(), b"second");

        assert!(manager.mark_pty_stopped(second_generation));
        assert!(!manager.is_alive());
        assert!(manager.write_input(b"ignored").is_err());
    }

    #[test]
    fn failed_input_marks_current_pty_dead() {
        let manager = TerminalManager::new("test".to_string(), "/tmp".to_string());
        let writer: SharedPtyWriter = Arc::new(Mutex::new(Box::new(FailingWriter)));
        manager.activate_pty(writer);

        assert!(manager.write_input(b"data").is_err());
        assert!(!manager.is_alive());
    }

    #[test]
    fn closed_terminal_cannot_accept_input() {
        let manager = TerminalManager::new("test".to_string(), "/tmp".to_string());
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer: SharedPtyWriter =
            Arc::new(Mutex::new(Box::new(RecordingWriter(output.clone()))));
        manager.activate_pty(writer);

        manager.close();

        assert!(manager.is_closed());
        assert!(!manager.is_alive());
        assert!(manager.write_input(b"should-not-run").is_err());
        assert!(output.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_exec_cancels_pending_multiline_input() {
        let app = tauri::test::mock_app();
        let manager = Arc::new(TerminalManager::new(
            "test-multiline-cleanup".to_string(),
            "/tmp".to_string(),
        ));
        manager.state.lock().unwrap().shell = "/bin/sh".to_string();
        let mut pty_state = Some(
            manager
                .start_and_spawn_reader("test-multiline-cleanup", "/tmp", app.handle().clone())
                .expect("测试 PTY 应成功启动"),
        );

        manager.write_input(b"if true; then\r").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !manager.recent_output(20).contains("if true; then") {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("测试 Shell 未进入多行续写状态");

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let cancellation = Arc::new(crate::types::TerminalExecCancellation::default());
        let completion = crate::types::TerminalExecCompletion::new(Arc::clone(&cancellation));
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            crate::command_protocol::handle_exec(
                &manager,
                &mut pty_state,
                app.handle(),
                "echo command-after-multiline",
                None,
                response_tx,
                cancellation,
                completion,
                None,
            ),
        )
        .await
        .expect("Agent 命令在多行清理后仍未完成");

        let response = response_rx.await.expect("命令响应发送端意外关闭");
        assert!(!response.terminal_error, "{}", response.stderr);
        assert_eq!(response.exit_code, 0);
        assert!(response.stdout.contains("command-after-multiline"));
        assert!(manager.is_alive());
        assert!(pty_state.is_some());

        manager.deactivate_pty();
        shutdown_pty(pty_state.take().unwrap());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_exec_rebuilds_terminal_when_cleanup_cannot_restore_shell() {
        let app = tauri::test::mock_app();
        let temp = tempfile::tempdir().unwrap();
        let foreground_ready = temp.path().join("foreground-ready");
        let manager = Arc::new(TerminalManager::new(
            "test-cleanup-recovery".to_string(),
            "/tmp".to_string(),
        ));
        manager.state.lock().unwrap().shell = "/bin/sh".to_string();
        let mut pty_state = Some(
            manager
                .start_and_spawn_reader("test-cleanup-recovery", "/tmp", app.handle().clone())
                .expect("测试 PTY 应成功启动"),
        );

        let blocking_command = format!(
            "sh -c 'trap \"\" INT HUP TERM; printf ready > \"$1\"; exec sleep 60' _ {}\r",
            shell_quote(&foreground_ready.to_string_lossy())
        );
        manager.write_input(blocking_command.as_bytes()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !foreground_ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("测试前台任务未启动");

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let cancellation = Arc::new(crate::types::TerminalExecCancellation::default());
        let completion = crate::types::TerminalExecCompletion::new(Arc::clone(&cancellation));
        tokio::time::timeout(
            std::time::Duration::from_secs(8),
            crate::command_protocol::handle_exec(
                &manager,
                &mut pty_state,
                app.handle(),
                "echo command-after-recovery",
                None,
                response_tx,
                cancellation,
                completion,
                None,
            ),
        )
        .await
        .expect("旧终端清理失败后没有完成重建执行");

        let response = response_rx.await.expect("命令响应发送端意外关闭");
        assert!(!response.terminal_error, "{}", response.stderr);
        assert_eq!(response.exit_code, 0);
        assert!(response.stdout.contains("command-after-recovery"));
        assert!(manager.is_alive());
        assert!(pty_state.is_some());

        manager.deactivate_pty();
        shutdown_pty(pty_state.take().unwrap());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_terminal_finishes_running_command_wait() {
        let app = tauri::test::mock_app();
        let temp = tempfile::tempdir().unwrap();
        let ready_path = temp.path().join("command-ready");
        let manager = Arc::new(TerminalManager::new(
            "test-close-command".to_string(),
            "/tmp".to_string(),
        ));
        manager.state.lock().unwrap().shell = "/bin/sh".to_string();
        let pty_state = manager
            .start_and_spawn_reader("test-close-command", "/tmp", app.handle().clone())
            .expect("测试 PTY 应成功启动");
        let command = format!(
            "sh -c 'printf ready > \"$1\"; exec sleep 60' _ {}",
            shell_quote(&ready_path.to_string_lossy())
        );
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let cancellation = Arc::new(crate::types::TerminalExecCancellation::default());
        let completion = crate::types::TerminalExecCompletion::new(Arc::clone(&cancellation));
        let task_manager = Arc::clone(&manager);
        let app_handle = app.handle().clone();
        let command_task = tokio::spawn(async move {
            let mut pty_state = Some(pty_state);
            crate::command_protocol::handle_exec(
                &task_manager,
                &mut pty_state,
                &app_handle,
                &command,
                None,
                response_tx,
                cancellation,
                completion,
                None,
            )
            .await;
            assert!(pty_state.is_none(), "关闭后不应保留旧 PTY");
        });

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !ready_path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("测试命令未启动");

        let closed_at = std::time::Instant::now();
        manager.close();
        let response = tokio::time::timeout(std::time::Duration::from_secs(2), response_rx)
            .await
            .expect("关闭终端后命令响应仍在等待")
            .expect("命令响应发送端意外关闭");
        assert!(response.terminal_error);
        assert!(response.stderr.contains("终端已关闭"));
        tokio::time::timeout(std::time::Duration::from_secs(2), command_task)
            .await
            .expect("关闭终端后命令处理任务仍在等待")
            .expect("命令处理任务异常退出");
        assert!(
            closed_at.elapsed() < std::time::Duration::from_secs(2),
            "关闭终端后命令等待未及时结束"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_pty_reaps_shell_process() {
        let ps = start_pty(
            "test",
            "/tmp",
            "/bin/sh",
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let pid = ps.child.process_id().unwrap() as libc::pid_t;

        shutdown_pty(ps);

        let result = unsafe { libc::kill(pid, 0) };
        assert_eq!(result, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_pty_stops_foreground_command_group() {
        let temp = tempfile::tempdir().unwrap();
        let pid_path = temp.path().join("foreground.pid");
        let ps = start_pty(
            "test",
            "/tmp",
            "/bin/sh",
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        {
            let mut writer = ps.writer.lock().unwrap();
            let command = format!(
                "sh -c 'echo $$ > \"$1\"; exec sleep 60' _ {}\r",
                shell_quote(&pid_path.to_string_lossy())
            );
            writer.write_all(command.as_bytes()).unwrap();
            writer.flush().unwrap();
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !pid_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let foreground_pid = std::fs::read_to_string(&pid_path)
            .expect("前台命令未启动")
            .trim()
            .parse::<libc::pid_t>()
            .unwrap();
        assert_eq!(unsafe { libc::kill(foreground_pid, 0) }, 0);

        shutdown_pty(ps);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let result = unsafe { libc::kill(foreground_pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "关闭 PTY 后前台命令仍在运行: {foreground_pid}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
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
