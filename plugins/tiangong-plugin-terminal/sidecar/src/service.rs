//! PTY 会话服务：请求-响应操作 + 输出流通知。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use portable_pty::{ChildKiller, CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::{Deserialize, Serialize};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, PROTOCOL_VERSION, Request, RequestInvocationContext, Response,
};
use tiangong_plugin_sidecar::server::emit_notification;

use crate::persist;

/// 通知通道：PTY 输出流（负载 JSON 见 `OutputNotification`）。
pub const CHANNEL_OUTPUT: &str = "terminal.output";
/// 通知通道：会话退出（负载 JSON 见 `ExitNotification`）。
pub const CHANNEL_EXIT: &str = "terminal.exit";

/// PTY 输出节流：读取线程按此间隔批量推送（约 60fps 上限）。
const OUTPUT_FLUSH_INTERVAL_MS: u64 = 16;
/// 会话最近输出环形缓冲上限（字节）：UI 重新附着时重放历史（含控制序列原样字节）。
const HISTORY_LIMIT_BYTES: usize = 128 * 1024;
/// 轮询 PTY 输出的间隔。
const COMMAND_POLL_INTERVAL_MS: u64 = 50;
/// 登录 shell 清理残留输入并确认重新接管终端的最长等待时间。
const SHELL_READY_TIMEOUT_SECS: u64 = 3;
/// 最后一个 PTY 会话结束后保留短暂窗口，让关闭响应完成并容纳紧邻的新建请求。
const SIDECAR_IDLE_EXIT_SECS: u64 = 5;
/// 内部命令边界标记公共前缀；这些行不得显示给用户。
const MARKER_PREFIX: &str = "__TIANGONG_";

pub struct TerminalService {
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    last_activity: Arc<Mutex<Option<Instant>>>,
    sequence: Mutex<u64>,
    spawn_lock: Mutex<()>,
    /// 进行中的 Agent 工具调用（invocation_id → 执行终端）：宿主超时或
    /// 会话取消时经 Cancel 帧中断等待，本表把取消定位到具体 PTY。
    active_tools: Arc<Mutex<HashMap<String, String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    Idle,
    Reserved,
    Running,
    Interactive,
}

struct PtySession {
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// 同一终端内的 Agent 命令串行执行，避免边界标记相互穿插。
    exec_lock: Arc<tokio::sync::Mutex<()>>,
    /// 宿主会话标识：终端跟会话走与恢复的锚点（同 scope 取最新会话）。
    scope_id: Option<String>,
    /// 创建序号：单调递增，同 scope 多会话时分辨最新。
    sequence: u64,
    /// Agent 命令选择与交互状态；选择时只复用空闲终端。
    phase: SessionPhase,
    /// 原始 PTY 输出：仅供命令边界识别与结果收集，包含内部标记。
    raw_history: Vec<u8>,
    /// 原始缓冲首字节在会话输出流中的绝对偏移。
    raw_history_start: u64,
    /// 原始输出累计字节数，不受环形缓冲截断影响。
    raw_bytes_total: u64,
    /// 用户可见输出：过滤内部标记后供 UI 重放。
    display_history: Vec<u8>,
    /// xterm 回传的当前可见画面，供 terminal_send 返回交互结果。
    screen_snapshot: String,
    screen_updates: u64,
    /// 输出持久化日志（按 scope 分文件）：打开失败为 None（优雅降级）。
    logger: Option<Arc<persist::OutputLogger>>,
}

#[derive(Debug, Clone, Serialize)]
struct OutputNotification<'a> {
    session_id: &'a str,
    data: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct ExitNotification<'a> {
    session_id: &'a str,
    exit_code: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnRequest {
    /// App 实例编号。UI 创建终端时由调用方指定，保证 App 与 PTY 一一对应。
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    /// 宿主会话标识：终端跟随会话切换的锚点。
    #[serde(default)]
    scope_id: Option<String>,
    /// Agent 选择后立即预留，避免并行命令复用同一终端。
    #[serde(default)]
    reserve: bool,
    #[serde(default = "default_cols")]
    cols: u16,
    #[serde(default = "default_rows")]
    rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecRequest {
    session_id: String,
    #[serde(default)]
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    interactive: bool,
    #[serde(default)]
    cwd: Option<String>,
}

/// 用户默认交互 shell（SHELL 环境变量，缺省回退 sh/cmd）。
fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(windows) {
            "cmd".to_string()
        } else {
            "sh".to_string()
        }
    })
}

/// 登录 shell 启动参数（对齐原版终端：bash/zsh 用 --login，sh 用 -l）。
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

fn shell_ready_probe(marker: &str) -> String {
    if cfg!(windows) {
        // cmd.exe 会把单引号当普通字符，且 ConPTY 会回显输入。用 ^ 拆开
        // 输入中的 marker；cmd 输出时会去掉 ^，因此完整 marker 只来自结果。
        let suffix = marker.strip_prefix(MARKER_PREFIX).unwrap_or(marker);
        format!("echo {MARKER_PREFIX}^{suffix}\r")
    } else {
        format!("echo '{marker}'\r")
    }
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

#[derive(Debug, Serialize)]
struct SpawnResponse {
    session_id: String,
    /// 首批输出（shell 提示符等）：随响应返回给 UI 作渲染基线。
    /// 冷启动窗口内宿主通知监听可能尚未连上（sidecar 通知无订阅者即
    /// 丢弃），首批走通知会永久丢失——终端黑屏的根因；随响应走则
    /// 不依赖通知时序。首批已落盘/入历史，聚合线程不再重发。
    #[serde(skip_serializing_if = "Option::is_none")]
    boot_output: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExecResponse {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
    cwd_after: String,
    interactive_mode: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest {
    session_id: String,
    data: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenUpdateRequest {
    session_id: String,
    screen: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcquireRequest {
    scope_id: String,
}

#[derive(Debug, Serialize)]
struct AcquireResponse {
    session_id: Option<String>,
    history: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    scope_id: String,
    session_id: String,
    input: String,
    #[serde(default)]
    wait: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SendResponse {
    session_id: String,
    stdout: String,
    exit_code: i32,
    interactive_mode: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeRequest {
    session_id: String,
    cols: u16,
    rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionIdRequest {
    session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseRequest {
    #[serde(default)]
    session_id: Option<String>,
    scope_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FindRequest {
    scope_id: String,
    /// App 工具打开时携带的 PTY 实例编号；存在时必须精确附着。
    #[serde(default)]
    session_id: Option<String>,
}

/// terminalFind 结果：会话不存在时 session_id 为 null（调用方据此新建）。
#[derive(Debug, Serialize)]
struct FindResponse {
    session_id: Option<String>,
    history: String,
}

#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

impl TerminalService {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            last_activity: Arc::new(Mutex::new(None)),
            sequence: Mutex::new(0),
            spawn_lock: Mutex::new(()),
            active_tools: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 登记一次执行中的工具调用；返回的守卫在正常完成或 future 被丢弃
    /// （宿主超时/取消后的 select 中断）时移除登记。
    fn track_active_tool(&self, invocation_id: &str, terminal_id: &str) -> ActiveToolGuard {
        self.active_tools
            .lock()
            .expect("活动工具表锁损坏")
            .insert(invocation_id.to_string(), terminal_id.to_string());
        ActiveToolGuard {
            active_tools: Arc::clone(&self.active_tools),
            invocation_id: invocation_id.to_string(),
        }
    }

    /// terminal 不随 App 常驻：首次业务请求由宿主按需拉起，最后一个 PTY
    /// 会话结束且空闲窗口届满后退出；共享连接会在下次操作时重新启动。
    pub fn start_idle_exit_monitor(&self) {
        let sessions = Arc::clone(&self.sessions);
        let last_activity = Arc::clone(&self.last_activity);
        if let Err(error) = std::thread::Builder::new()
            .name("terminal-idle-exit".to_string())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(250));
                    let has_sessions = sessions
                        .lock()
                        .map(|sessions| !sessions.is_empty())
                        .unwrap_or(true);
                    if has_sessions {
                        continue;
                    }
                    let idle = last_activity
                        .lock()
                        .ok()
                        .and_then(|activity| *activity)
                        .is_some_and(|activity| {
                            activity.elapsed() >= Duration::from_secs(SIDECAR_IDLE_EXIT_SECS)
                        });
                    if idle {
                        tracing::info!("terminal sidecar 已无 PTY 会话，退出空闲进程");
                        std::process::exit(0);
                    }
                }
            })
        {
            tracing::warn!(%error, "启动 terminal 空闲退出监视失败");
        }
    }

    fn mark_activity(&self) {
        *self.last_activity.lock().expect("终端活动时间锁损坏") = Some(Instant::now());
    }

    fn next_sequence(&self) -> u64 {
        let mut sequence = self.sequence.lock().expect("会话序号锁损坏");
        *sequence += 1;
        *sequence
    }

    fn spawn_session(&self, request: SpawnRequest) -> Result<SpawnResponse> {
        let _spawn_guard = self.spawn_lock.lock().expect("终端创建锁损坏");
        let requested_session_id = request
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if let Some(session_id) = requested_session_id.as_deref() {
            let mut sessions = self.sessions.lock().expect("会话表锁损坏");
            if let Some(session) = sessions.get_mut(session_id) {
                if session.scope_id.as_deref() != request.scope_id.as_deref() {
                    bail!("终端 {session_id} 已属于其他会话");
                }
                if request.reserve {
                    if session.phase != SessionPhase::Idle {
                        bail!("终端 {session_id} 正在使用");
                    }
                    session.phase = SessionPhase::Reserved;
                }
                return Ok(SpawnResponse {
                    session_id: session_id.to_string(),
                    boot_output: Some(
                        String::from_utf8_lossy(&session.display_history).to_string(),
                    ),
                });
            }
        }

        // 编号全局唯一（scru128）：sidecar 换代（崩溃重启/空闲退出/升级）
        // 后序号不回卷，Agent 持有的旧编号只会明确失败，不会误命中新终端。
        // sequence 与编号身份无关，仅用于同 scope 多终端时分辨最新。
        let (sequence, session_id) = loop {
            let sequence = self.next_sequence();
            let session_id = requested_session_id
                .clone()
                .unwrap_or_else(|| format!("tty-{}", scru128::new()));
            if !self
                .sessions
                .lock()
                .expect("会话表锁损坏")
                .contains_key(&session_id)
            {
                break (sequence, session_id);
            }
        };
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(PtySize {
                rows: request.rows,
                cols: request.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("创建 PTY 失败")?;

        // 脚本走 shell -c；命令直接执行
        let mut command = if let Some(script) = request.script.as_deref() {
            let shell = default_shell();
            let mut builder = CommandBuilder::new(shell);
            builder.arg("-c");
            builder.arg(script);
            builder
        } else if request.cmd.is_empty() {
            // 登录 shell 启动（对齐原版终端与 Terminal.app 行为）：source
            // /etc/profile 与 ~/.zprofile 等，拿到用户真实 PATH；并设置
            // TERM 让 zsh/zle 以全功能终端运行（否则语法高亮/补全降级错乱）。
            let shell = default_shell();
            let mut builder = CommandBuilder::new(&shell);
            for arg in login_shell_args(&shell) {
                builder.arg(arg);
            }
            builder.env("TERM", "xterm-256color");
            builder.env("COLORTERM", "truecolor");
            builder
        } else {
            let mut builder = CommandBuilder::new(&request.cmd);
            builder.env("TERM", "xterm-256color");
            builder
        };
        for arg in &request.args {
            command.arg(arg);
        }
        // cwd 失效（目录被删等）不阻断会话创建：回退到继承进程工作目录
        if let Some(cwd) = request
            .cwd
            .as_deref()
            .filter(|cwd| std::path::Path::new(cwd).is_dir())
        {
            command.cwd(cwd);
        }

        let mut child = pair
            .slave
            .spawn_command(command)
            .context("启动 PTY 子进程失败")?;
        drop(pair.slave);

        let killer = child.clone_killer();
        // 输出持久化（按 scope 分文件）：应用重启后回填该会话的终端历史
        let logger = request
            .scope_id
            .as_deref()
            .and_then(persist::scope_log_path)
            .and_then(persist::OutputLogger::open)
            .map(Arc::new);
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("克隆 PTY 读取端失败")?;
        let writer = pair.master.take_writer().context("获取 PTY 写入端失败")?;
        let master = pair.master;

        // 必须先登记会话再启动输出搬运；否则启动提示符可能先于登记到达，
        // 原始输出和重放历史都会永久丢失。
        self.sessions.lock().expect("会话表锁损坏").insert(
            session_id.clone(),
            PtySession {
                writer,
                killer,
                master,
                exec_lock: Arc::new(tokio::sync::Mutex::new(())),
                scope_id: request.scope_id,
                sequence,
                phase: if request.reserve {
                    SessionPhase::Reserved
                } else {
                    SessionPhase::Idle
                },
                raw_history: Vec::new(),
                raw_history_start: 0,
                raw_bytes_total: 0,
                display_history: Vec::new(),
                screen_snapshot: String::new(),
                screen_updates: 0,
                logger,
            },
        );

        // 输出管道：原始流保留给命令完成协议，用户可见流先过滤内部
        // marker 和包含 marker 的 shell 回显，再落盘、推送与重放。
        let output_session = session_id.clone();
        let output_sessions = Arc::clone(&self.sessions);
        let boot_sessions = Arc::clone(&self.sessions);
        let (chunk_tx, chunk_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(bytes) => {
                        if chunk_tx.send(buffer[..bytes].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let mut output_filter = RawOutputFilter::new();
        // 首批提示符随响应返回，避免 UI 订阅尚未建立时永久丢帧。
        let boot_output = match chunk_rx.recv_timeout(Duration::from_millis(40)) {
            Ok(first) => {
                record_raw_output(&boot_sessions, &output_session, &first);
                let filtered = output_filter.filter(&String::from_utf8_lossy(&first));
                record_display_output(&boot_sessions, &output_session, filtered.as_bytes());
                (!filtered.is_empty()).then_some(filtered)
            }
            Err(_) => None,
        };
        std::thread::spawn(move || {
            while let Ok(first) = chunk_rx.recv() {
                let mut pending = first;
                while let Ok(more) =
                    chunk_rx.recv_timeout(Duration::from_millis(OUTPUT_FLUSH_INTERVAL_MS))
                {
                    pending.extend_from_slice(&more);
                    if pending.len() >= 64 * 1024 {
                        break;
                    }
                }
                record_raw_output(&output_sessions, &output_session, &pending);
                let filtered = output_filter.filter(&String::from_utf8_lossy(&pending));
                if !filtered.is_empty() {
                    publish_display_output(&output_sessions, &output_session, &filtered);
                }
            }
        });

        // 等待线程：退出通知 + 会话出表（find 不再命中死会话，
        // write/resize 自然返回「会话不存在」）。
        let exit_session = session_id.clone();
        let exit_sequence = sequence;
        let exit_sessions = Arc::clone(&self.sessions);
        std::thread::spawn(move || {
            let status = child.wait();
            let exit_code = status.as_ref().ok().map(|status| status.exit_code());
            let payload = serde_json::to_string(&ExitNotification {
                session_id: &exit_session,
                exit_code,
            })
            .unwrap_or_default();
            emit_notification(CHANNEL_EXIT, payload);
            let mut sessions = exit_sessions.lock().expect("会话表锁损坏");
            if sessions
                .get(&exit_session)
                .is_some_and(|session| session.sequence == exit_sequence)
            {
                sessions.remove(&exit_session);
            }
        });

        Ok(SpawnResponse {
            session_id,
            boot_output,
        })
    }

    fn with_session<R>(
        &self,
        session_id: &str,
        operation: impl FnOnce(&mut PtySession) -> Result<R>,
    ) -> Result<R> {
        let mut sessions = self.sessions.lock().expect("会话表锁损坏");
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        operation(session)
    }

    fn kill_session(&self, request: SessionIdRequest) -> Result<OkResponse> {
        let removed = self
            .sessions
            .lock()
            .expect("会话表锁损坏")
            .remove(&request.session_id);
        match removed {
            Some(mut session) => {
                let _ = session.killer.kill();
                Ok(OkResponse { ok: true })
            }
            None => Ok(OkResponse { ok: false }),
        }
    }

    fn close_session(&self, request: CloseRequest) -> Result<OkResponse> {
        let (removed, has_other_scope_sessions) = {
            let mut sessions = self.sessions.lock().expect("会话表锁损坏");
            let removed = if let Some(session_id) = request.session_id.as_deref() {
                if let Some(session) = sessions.get(session_id)
                    && session.scope_id.as_deref() != Some(request.scope_id.as_str())
                {
                    bail!("终端 {session_id} 不属于当前会话");
                }
                sessions.remove(session_id)
            } else {
                None
            };
            let has_other_scope_sessions = sessions
                .values()
                .any(|session| session.scope_id.as_deref() == Some(request.scope_id.as_str()));
            (removed, has_other_scope_sessions)
        };
        if let Some(mut session) = removed {
            let _ = session.killer.kill();
        }
        if !has_other_scope_sessions {
            persist::clear_scope_log(&request.scope_id).context("清理终端恢复记录失败")?;
        }
        Ok(OkResponse { ok: true })
    }

    fn acquire_session(&self, request: AcquireRequest) -> AcquireResponse {
        let mut sessions = self.sessions.lock().expect("会话表锁损坏");
        let had_live_terminal = sessions
            .values()
            .any(|session| session.scope_id.as_deref() == Some(request.scope_id.as_str()));
        let selected = sessions
            .iter()
            .filter(|(_, session)| {
                session.scope_id.as_deref() == Some(request.scope_id.as_str())
                    && session.phase == SessionPhase::Idle
            })
            .min_by_key(|(_, session)| session.sequence)
            .map(|(session_id, _)| session_id.clone());
        let Some(session_id) = selected else {
            return AcquireResponse {
                session_id: None,
                history: String::new(),
                reason: if had_live_terminal {
                    "all_busy".to_string()
                } else {
                    "no_available".to_string()
                },
            };
        };
        let session = sessions.get_mut(&session_id).expect("刚选择的终端不存在");
        session.phase = SessionPhase::Reserved;
        AcquireResponse {
            session_id: Some(session_id),
            history: String::from_utf8_lossy(&session.display_history).to_string(),
            reason: "reused_idle".to_string(),
        }
    }

    fn release_session(&self, request: SessionIdRequest) -> Result<OkResponse> {
        self.with_session(&request.session_id, |session| {
            if session.phase == SessionPhase::Reserved {
                session.phase = SessionPhase::Idle;
            }
            Ok(OkResponse { ok: true })
        })
    }

    fn update_screen(&self, request: ScreenUpdateRequest) -> Result<OkResponse> {
        self.with_session(&request.session_id, |session| {
            session.screen_snapshot = request.screen;
            session.screen_updates = session.screen_updates.saturating_add(1);
            Ok(OkResponse { ok: true })
        })
    }

    async fn send_to_session(&self, request: SendRequest) -> Result<SendResponse> {
        let data = decode_terminal_escapes(&request.input);
        let (session_id, start_offset, baseline_updates) = {
            let mut sessions = self.sessions.lock().expect("会话表锁损坏");
            let selected = sessions
                .get(&request.session_id)
                .filter(|session| session.scope_id.as_deref() == Some(request.scope_id.as_str()))
                .map(|_| request.session_id.clone())
                .ok_or_else(|| anyhow::anyhow!("目标终端不存在或不属于当前会话"))?;
            let session = sessions.get_mut(&selected).expect("刚选择的终端不存在");
            let start_offset = session.raw_bytes_total;
            let baseline_updates = session.screen_updates;
            session
                .writer
                .write_all(data.as_bytes())
                .context("写入交互输入失败")?;
            session.writer.flush().context("刷新交互输入失败")?;
            (selected, start_offset, baseline_updates)
        };

        let deadline = Instant::now() + Duration::from_secs(request.wait.unwrap_or(3).max(1));
        let stable_window = Duration::from_millis(300);
        let mut last_raw_total = start_offset;
        let mut last_screen_updates = baseline_updates;
        let mut last_change_at = None;
        let mut saw_screen_update = false;
        loop {
            let (raw_total, screen_updates) = self.with_session(&session_id, |session| {
                Ok((session.raw_bytes_total, session.screen_updates))
            })?;
            if raw_total != last_raw_total {
                last_raw_total = raw_total;
                last_change_at = Some(Instant::now());
            }
            if screen_updates != last_screen_updates {
                last_screen_updates = screen_updates;
                last_change_at = Some(Instant::now());
                saw_screen_update = true;
            }
            let required_stability = if saw_screen_update {
                stable_window
            } else {
                stable_window * 2
            };
            if last_change_at.is_some_and(|changed| changed.elapsed() >= required_stability)
                || Instant::now() >= deadline
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL_MS)).await;
        }

        let raw = self.raw_output_since(&session_id, start_offset)?;
        let (screen, screen_updates, interactive_mode) =
            self.with_session(&session_id, |session| {
                Ok((
                    session.screen_snapshot.clone(),
                    session.screen_updates,
                    session.phase == SessionPhase::Interactive,
                ))
            })?;
        let raw_output = visible_text_from_raw(&raw);
        Ok(SendResponse {
            session_id,
            stdout: if screen_updates > baseline_updates && !screen.trim().is_empty() {
                screen
            } else if !raw_output.trim().is_empty() {
                raw_output
            } else {
                screen
            },
            exit_code: 0,
            interactive_mode,
        })
    }

    async fn exec_in_session(&self, request: ExecRequest) -> Result<ExecResponse> {
        let command = command_from_request(&request)?;
        let exec_lock = self.with_session(&request.session_id, |session| {
            Ok(Arc::clone(&session.exec_lock))
        })?;
        let _guard = exec_lock.lock().await;
        self.with_session(&request.session_id, |session| {
            if session.phase == SessionPhase::Interactive {
                bail!("终端正在运行交互程序");
            }
            session.phase = SessionPhase::Running;
            Ok(())
        })?;

        let result = async {
            // 与正式版一致：先取消 shell 中的残留输入或前台命令，再用隐藏
            // 探针确认登录 shell 已重新接管，最后才发送 Agent 的真实命令。
            self.prepare_shell_for_command(&request.session_id).await?;
            if request.interactive {
                self.exec_interactive(&request.session_id, &command, request.timeout)
                    .await
            } else {
                self.exec_non_interactive(&request, &command).await
            }
        }
        .await;
        let next_phase = if result
            .as_ref()
            .is_ok_and(|response| response.interactive_mode)
        {
            SessionPhase::Interactive
        } else {
            SessionPhase::Idle
        };
        let _ = self.with_session(&request.session_id, |session| {
            session.phase = next_phase;
            Ok(())
        });
        result
    }

    async fn prepare_shell_for_command(&self, session_id: &str) -> Result<()> {
        let initial_deadline = Instant::now() + Duration::from_secs(SHELL_READY_TIMEOUT_SECS);
        while self.with_session(session_id, |session| Ok(session.raw_bytes_total == 0))?
            && Instant::now() < initial_deadline
        {
            tokio::time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL_MS)).await;
        }
        // ConPTY 可能先交付半段提示符；此时立即写入 Ctrl+C 会让刚启动的
        // cmd.exe 退出。等待一个短稳定窗口后再清理残留输入。
        tokio::time::sleep(Duration::from_millis(300)).await;

        let marker = format!("__TIANGONG_READY_{}__", scru128::new());
        let start_offset = self.with_session(session_id, |session| Ok(session.raw_bytes_total))?;
        self.with_session(session_id, |session| {
            session
                .writer
                .write_all(b"\x03")
                .context("清理终端残留输入失败")?;
            session.writer.flush().context("刷新终端中断输入失败")
        })?;

        // 提示符空闲时部分 shell 处理 Ctrl+C 不会产生完整新行。短暂等待
        // 任意输出变化；即使没有回显也继续由下面的精确探针判断是否就绪。
        let interrupt_deadline = Instant::now() + Duration::from_millis(300);
        loop {
            let raw = self.raw_output_since(session_id, start_offset)?;
            if !raw.is_empty() || Instant::now() >= interrupt_deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL_MS)).await;
        }

        let probe_offset = self.with_session(session_id, |session| Ok(session.raw_bytes_total))?;
        self.with_session(session_id, |session| {
            session
                .writer
                .write_all(shell_ready_probe(&marker).as_bytes())
                .context("发送 Shell 就绪探针失败")?;
            session.writer.flush().context("刷新 Shell 就绪探针失败")
        })?;

        let probe_deadline = Instant::now() + Duration::from_secs(SHELL_READY_TIMEOUT_SECS);
        loop {
            let raw = self.raw_output_since(session_id, probe_offset)?;
            let mut processor = persist::TerminalLineProcessor::new();
            let mut lines = processor.process(&String::from_utf8_lossy(&raw));
            let current = processor.current_line();
            if !current.trim().is_empty() {
                lines.push(current);
            }
            let probe_completed = if cfg!(windows) {
                String::from_utf8_lossy(&raw).contains(&marker)
            } else {
                lines.iter().any(|line| line.trim() == marker)
            };
            if probe_completed {
                return Ok(());
            }
            if Instant::now() >= probe_deadline {
                let output = String::from_utf8_lossy(&raw);
                let reversed_tail = output.chars().rev().take(2048).collect::<String>();
                let output_tail = reversed_tail.chars().rev().collect::<String>();
                tracing::warn!(
                    session_id,
                    probe_output = ?output_tail,
                    "等待 Shell 就绪超时，关闭失效终端会话"
                );
                let _ = self.kill_session(SessionIdRequest {
                    session_id: session_id.to_string(),
                });
                bail!("等待 Shell 就绪超时，已关闭失效终端");
            }
            tokio::time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL_MS)).await;
        }
    }

    async fn exec_non_interactive(
        &self,
        request: &ExecRequest,
        command: &str,
    ) -> Result<ExecResponse> {
        let marker_id = scru128::new().to_string();
        let markers = CommandMarkers::new(&marker_id);
        let prepared = prepare_non_interactive_command(command, &markers)?;
        let start_offset =
            self.with_session(&request.session_id, |session| Ok(session.raw_bytes_total))?;

        // shell 回显的是内部 wrapper，因此先把用户实际命令写入可见输出；
        // reader 会过滤包含 marker 的 wrapper 回显和边界行。
        publish_display_output(
            &self.sessions,
            &request.session_id,
            &format!("{}\r\n", command.trim_end()),
        );
        self.with_session(&request.session_id, |session| {
            session
                .writer
                .write_all(prepared.input.as_bytes())
                .context("写入终端命令失败")?;
            session.writer.flush().context("刷新终端命令失败")
        })?;

        let deadline = request
            .timeout
            .map(|timeout| Instant::now() + Duration::from_secs(timeout.max(1)));
        let mut exit_code_seen_at = None;
        loop {
            let raw = self.raw_output_since(&request.session_id, start_offset)?;
            let parsed = parse_command_output(&raw, &markers);
            if parsed.completed {
                // end marker 已闭合后让登录 shell 完成提示符绘制，避免紧随其后的
                // terminal_send 与 wrapper 收尾交错，造成输入多字或终端状态异常。
                tokio::time::sleep(Duration::from_millis(100)).await;
                return Ok(parsed.into_response(false));
            }
            if parsed.exit_code.is_some() {
                let seen_at = exit_code_seen_at.get_or_insert_with(Instant::now);
                // 少数 TTY 程序会污染 end marker。退出码已出现但 500ms 内仍
                // 未见 end 时按已完成兜底，不能固定等到整个命令超时。
                if seen_at.elapsed() >= Duration::from_millis(500) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    return Ok(parsed.into_response(false));
                }
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                self.with_session(&request.session_id, |session| {
                    session
                        .writer
                        .write_all(b"\x03")
                        .context("中断超时命令失败")?;
                    session.writer.flush().context("刷新中断输入失败")
                })?;
                return self
                    .collect_after_interrupt(&request.session_id, start_offset, &markers)
                    .await;
            }
            tokio::time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL_MS)).await;
        }
    }

    async fn collect_after_interrupt(
        &self,
        session_id: &str,
        start_offset: u64,
        markers: &CommandMarkers,
    ) -> Result<ExecResponse> {
        let grace_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let raw = self.raw_output_since(session_id, start_offset)?;
            let parsed = parse_command_output(&raw, markers);
            if parsed.completed || Instant::now() >= grace_deadline {
                let mut response = parsed.into_response(true);
                response.exit_code = -1;
                response.stderr = "命令执行超时".to_string();
                return Ok(response);
            }
            tokio::time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL_MS)).await;
        }
    }

    async fn exec_interactive(
        &self,
        session_id: &str,
        command: &str,
        wait_secs: Option<u64>,
    ) -> Result<ExecResponse> {
        let start_offset = self.with_session(session_id, |session| Ok(session.raw_bytes_total))?;
        self.with_session(session_id, |session| {
            session
                .writer
                .write_all(format!("{}\r", command.trim_end()).as_bytes())
                .context("写入交互命令失败")?;
            session.writer.flush().context("刷新交互命令失败")
        })?;

        let deadline = Instant::now() + Duration::from_secs(wait_secs.unwrap_or(3).max(1));
        let mut changed_at = None;
        let mut last_len = 0;
        loop {
            let raw = self.raw_output_since(session_id, start_offset)?;
            if raw.len() != last_len {
                last_len = raw.len();
                changed_at = Some(Instant::now());
            }
            if changed_at.is_some_and(|changed| changed.elapsed() >= Duration::from_millis(300))
                || Instant::now() >= deadline
            {
                let mut processor = persist::TerminalLineProcessor::new();
                let mut lines = processor.process(&String::from_utf8_lossy(&raw));
                let current = processor.current_line();
                if !current.trim().is_empty() {
                    lines.push(current);
                }
                return Ok(ExecResponse {
                    stdout: lines.join("\n"),
                    stderr: String::new(),
                    exit_code: 0,
                    timed_out: false,
                    cwd_after: String::new(),
                    interactive_mode: true,
                });
            }
            tokio::time::sleep(Duration::from_millis(COMMAND_POLL_INTERVAL_MS)).await;
        }
    }

    fn raw_output_since(&self, session_id: &str, offset: u64) -> Result<Vec<u8>> {
        self.with_session(session_id, |session| {
            let available_start = offset.max(session.raw_history_start);
            let index = available_start
                .saturating_sub(session.raw_history_start)
                .min(session.raw_history.len() as u64) as usize;
            Ok(session.raw_history[index..].to_vec())
        })
    }

    /// 按宿主会话标识找最近创建的活跃终端：UI 跟随会话切换时先恢复
    /// 既有 PTY（含最近输出历史），没有再新建。无存活会话时返回磁盘
    /// 日志尾部（应用重启后回填历史，UI 据此新建 shell 并重放）。
    fn find_by_scope(&self, request: &FindRequest) -> FindResponse {
        let sessions = self.sessions.lock().expect("会话表锁损坏");
        if let Some(session_id) = request.session_id.as_deref()
            && let Some(session) = sessions.get(session_id)
            && session.scope_id.as_deref() == Some(request.scope_id.as_str())
        {
            return FindResponse {
                session_id: Some(session_id.to_string()),
                history: String::from_utf8_lossy(&session.display_history).to_string(),
            };
        }
        if request.session_id.is_some() {
            drop(sessions);
            let history = persist::scope_log_path(&request.scope_id)
                .map(|path| persist::read_log_tail(&path, persist::LOG_TAIL_LINES))
                .unwrap_or_default()
                .join("\r\n");
            return FindResponse {
                session_id: None,
                history,
            };
        }
        if let Some((session_id, session)) = sessions
            .iter()
            .filter(|(_, session)| session.scope_id.as_deref() == Some(request.scope_id.as_str()))
            .max_by_key(|(_, session)| session.sequence)
        {
            return FindResponse {
                session_id: Some(session_id.clone()),
                history: String::from_utf8_lossy(&session.display_history).to_string(),
            };
        }
        drop(sessions);
        let history = persist::scope_log_path(&request.scope_id)
            .map(|path| persist::read_log_tail(&path, persist::LOG_TAIL_LINES))
            .unwrap_or_default()
            .join("\r\n");
        FindResponse {
            session_id: None,
            history,
        }
    }
}

/// 记录原始 PTY 输出，供命令边界检测和 stdout 收集。
fn record_raw_output(
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    session_id: &str,
    bytes: &[u8],
) {
    let mut sessions = sessions.lock().expect("会话表锁损坏");
    let Some(session) = sessions.get_mut(session_id) else {
        return;
    };
    session.raw_history.extend_from_slice(bytes);
    session.raw_bytes_total = session.raw_bytes_total.saturating_add(bytes.len() as u64);
    let overflow = session
        .raw_history
        .len()
        .saturating_sub(HISTORY_LIMIT_BYTES);
    if overflow > 0 {
        session.raw_history.drain(..overflow);
        session.raw_history_start = session.raw_history_start.saturating_add(overflow as u64);
    }
}

/// 记录过滤后的用户可见输出。日志写在会话表锁外，避免阻塞输入与 resize。
fn record_display_output(
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    session_id: &str,
    bytes: &[u8],
) {
    if bytes.is_empty() {
        return;
    }
    let logger = {
        let mut sessions = sessions.lock().expect("会话表锁损坏");
        let Some(session) = sessions.get_mut(session_id) else {
            return;
        };
        session.display_history.extend_from_slice(bytes);
        let overflow = session
            .display_history
            .len()
            .saturating_sub(HISTORY_LIMIT_BYTES);
        if overflow > 0 {
            session.display_history.drain(..overflow);
        }
        session.logger.clone()
    };
    if let Some(logger) = logger {
        logger.append(&String::from_utf8_lossy(bytes));
    }
}

fn publish_display_output(
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    session_id: &str,
    data: &str,
) {
    record_display_output(sessions, session_id, data.as_bytes());
    let payload =
        serde_json::to_string(&OutputNotification { session_id, data }).unwrap_or_default();
    emit_notification(CHANNEL_OUTPUT, payload);
}

fn contains_marker(text: &str) -> bool {
    text.contains(MARKER_PREFIX)
}

/// 行级 marker 过滤器：内部 wrapper 的 shell 回显和边界行不进入 UI。
struct RawOutputFilter {
    pending: String,
    suppress_until_newline: bool,
}

impl RawOutputFilter {
    fn new() -> Self {
        Self {
            pending: String::new(),
            suppress_until_newline: false,
        }
    }

    fn filter(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut result = String::new();
        while let Some((end, newline)) = next_output_boundary(&self.pending) {
            let segment = self.pending[..end].to_string();
            self.pending.drain(..end);
            if contains_marker(&segment) {
                self.suppress_until_newline = !newline;
            } else if !self.suppress_until_newline {
                result.push_str(&segment);
            }
            if newline {
                self.suppress_until_newline = false;
            }
        }
        if self.pending.is_empty() {
            return result;
        }
        if self.suppress_until_newline {
            return result;
        }
        if contains_marker(&self.pending) {
            if self.pending.len() > 8192 {
                result.push_str(&self.pending);
                self.pending.clear();
            }
            return result;
        }
        let split = marker_safe_split(&self.pending);
        if split > 0 {
            result.push_str(&self.pending[..split]);
            self.pending.drain(..split);
        }
        result
    }
}

fn next_output_boundary(value: &str) -> Option<(usize, bool)> {
    let newline = value.find('\n').map(|index| (index + 1, true));
    let bytes = value.as_bytes();
    let mut cursor = None;
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] != b'\x1b' || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let mut end = index + 2;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
            end += 1;
        }
        if end < bytes.len() && matches!(bytes[end], b'H' | b'f') {
            cursor = Some((end + 1, false));
            break;
        }
        index += 1;
    }
    match (newline, cursor) {
        (Some(newline), Some(cursor)) => Some(if newline.0 <= cursor.0 {
            newline
        } else {
            cursor
        }),
        (Some(boundary), None) | (None, Some(boundary)) => Some(boundary),
        (None, None) => None,
    }
}

fn marker_safe_split(value: &str) -> usize {
    for prefix_len in (1..MARKER_PREFIX.len()).rev() {
        if value.len() < prefix_len {
            continue;
        }
        let split = value.len() - prefix_len;
        if value.is_char_boundary(split) && MARKER_PREFIX.starts_with(&value[split..]) {
            return split;
        }
    }
    value.len()
}

impl Default for TerminalService {
    fn default() -> Self {
        Self::new()
    }
}

struct CommandMarkers {
    start: String,
    end: String,
    cwd: String,
    exit_code: String,
}

impl CommandMarkers {
    fn new(id: &str) -> Self {
        Self {
            start: format!("__TIANGONG_START_{id}__"),
            end: format!("__TIANGONG_END_{id}__"),
            cwd: format!("__TIANGONG_CWD_{id}__"),
            exit_code: format!("__TIANGONG_RC_{id}__"),
        }
    }
}

#[derive(Default)]
struct ParsedCommandOutput {
    stdout: String,
    exit_code: Option<i32>,
    cwd_after: String,
    completed: bool,
}

impl ParsedCommandOutput {
    fn into_response(self, timed_out: bool) -> ExecResponse {
        ExecResponse {
            stdout: self.stdout,
            stderr: String::new(),
            exit_code: self.exit_code.unwrap_or(if timed_out { -1 } else { 0 }),
            timed_out,
            cwd_after: self.cwd_after,
            interactive_mode: false,
        }
    }
}

fn command_from_request(request: &ExecRequest) -> Result<String> {
    let command = if let Some(script) = request.script.as_deref() {
        if script.trim().is_empty() {
            bail!("script 不能为空");
        }
        script.to_string()
    } else {
        if request.cmd.trim().is_empty() {
            bail!("cmd 不能为空");
        }
        // cmd 可含参数（与 command 插件的 run_command 语义一致）：先按引号
        // 感知规则拆分成程序名 + 内联参数，再逐词 quote 拼接，避免
        // "git status" 被整体 quote 成单个命令名导致找不到命令。
        let (program, inline_args) = split_command(request.cmd.trim());
        let mut command = shell_quote(&program);
        for arg in inline_args.iter().chain(request.args.iter()) {
            command.push(' ');
            command.push_str(&shell_quote(arg));
        }
        command
    };
    let Some(cwd) = request
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
    else {
        return Ok(command);
    };
    if cfg!(windows) {
        Ok(format!("cd /d {} && {}", shell_quote(cwd), command))
    } else {
        Ok(format!("cd {} && {}", shell_quote(cwd), command))
    }
}

/// 按引号感知规则拆分命令字符串为（程序名, 参数列表）。
///
/// 与 command 插件 sidecar 的 `split_command` 语义一致：单双引号成组、
/// 反斜杠转义，空白分隔；无引号的裸词按空白切分。
fn split_command(raw: &str) -> (String, Vec<String>) {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        return (raw.to_string(), Vec::new());
    }
    let cmd = parts.remove(0);
    (cmd, parts)
}

fn shell_quote(value: &str) -> String {
    if cfg!(windows) {
        if value.is_empty() || value.contains(' ') || value.contains('"') {
            format!("\"{}\"", value.replace('"', "\"\""))
        } else {
            value.to_string()
        }
    } else if value.is_empty()
        || value.contains(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '\'' | '"'
                        | '\\'
                        | '$'
                        | '`'
                        | '!'
                        | '*'
                        | '?'
                        | '['
                        | ']'
                        | '('
                        | ')'
                        | '{'
                        | '}'
                        | '|'
                        | '&'
                        | ';'
                        | '<'
                        | '>'
                        | '~'
                )
        })
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.to_string()
    }
}

/// terminal_send 接受 Agent 常用的字面控制键写法（如 `\x1b`、`\r`）。
fn decode_terminal_escapes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        let Some(escaped) = chars.next() else {
            output.push('\\');
            break;
        };
        match escaped {
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            '0' => output.push('\0'),
            'e' | 'E' => output.push('\x1b'),
            '\\' => output.push('\\'),
            'x' => {
                let digits = chars.clone().take(2).collect::<String>();
                if digits.len() == 2
                    && let Ok(value) = u8::from_str_radix(&digits, 16)
                {
                    chars.next();
                    chars.next();
                    output.push(char::from(value));
                } else {
                    output.push_str("\\x");
                }
            }
            'u' => {
                let digits = chars.clone().take(4).collect::<String>();
                if digits.len() == 4
                    && let Ok(value) = u32::from_str_radix(&digits, 16)
                    && let Some(value) = char::from_u32(value)
                {
                    for _ in 0..4 {
                        chars.next();
                    }
                    output.push(value);
                } else {
                    output.push_str("\\u");
                }
            }
            other => {
                output.push('\\');
                output.push(other);
            }
        }
    }
    output
}

fn visible_text_from_raw(raw: &[u8]) -> String {
    let mut processor = persist::TerminalLineProcessor::new();
    let mut lines = processor.process(&String::from_utf8_lossy(raw));
    let current = processor.current_line();
    if !current.trim().is_empty() {
        lines.push(current);
    }
    lines.join("\n")
}

struct PreparedCommand {
    input: String,
    /// Unix 下保持临时脚本存活到命令结束；drop 后自动删除。
    _file: Option<tempfile::NamedTempFile>,
}

#[cfg(not(windows))]
fn prepare_non_interactive_command(
    command: &str,
    markers: &CommandMarkers,
) -> Result<PreparedCommand> {
    let script = format!(
        "echo '{}'\n__tiangong_had_PAGER=${{PAGER+x}}\n__tiangong_old_PAGER=${{PAGER-}}\n__tiangong_had_GIT_PAGER=${{GIT_PAGER+x}}\n__tiangong_old_GIT_PAGER=${{GIT_PAGER-}}\n__tiangong_had_GH_PAGER=${{GH_PAGER+x}}\n__tiangong_old_GH_PAGER=${{GH_PAGER-}}\n__tiangong_had_LESS=${{LESS+x}}\n__tiangong_old_LESS=${{LESS-}}\nexport PAGER=cat GIT_PAGER=cat GH_PAGER=cat LESS=FRX\n{}\n__tiangong_rc=$?\nif [ -n \"$__tiangong_had_PAGER\" ]; then PAGER=\"$__tiangong_old_PAGER\"; export PAGER; else unset PAGER; fi\nif [ -n \"$__tiangong_had_GIT_PAGER\" ]; then GIT_PAGER=\"$__tiangong_old_GIT_PAGER\"; export GIT_PAGER; else unset GIT_PAGER; fi\nif [ -n \"$__tiangong_had_GH_PAGER\" ]; then GH_PAGER=\"$__tiangong_old_GH_PAGER\"; export GH_PAGER; else unset GH_PAGER; fi\nif [ -n \"$__tiangong_had_LESS\" ]; then LESS=\"$__tiangong_old_LESS\"; export LESS; else unset LESS; fi\nprintf '\\n{}'; pwd\necho '{}'$__tiangong_rc\necho '{}'\n",
        markers.start, command, markers.cwd, markers.exit_code, markers.end,
    );
    let mut file = tempfile::Builder::new()
        .prefix(&markers.start)
        .suffix(".sh")
        .tempfile()
        .context("创建终端命令临时文件失败")?;
    file.write_all(script.as_bytes())
        .context("写入终端命令临时文件失败")?;
    file.flush().context("刷新终端命令临时文件失败")?;
    let path = file.path().to_string_lossy();
    Ok(PreparedCommand {
        // marker 放在输入行最前面，reader 从首个 chunk 起就会暂存并过滤
        // shell/ZLE 回显，不会因路径换行或语法高亮把内部 source 命令漏到 UI。
        input: format!("__TIANGONG_= . {}\r", shell_quote(&path)),
        _file: Some(file),
    })
}

#[cfg(windows)]
fn prepare_non_interactive_command(
    command: &str,
    markers: &CommandMarkers,
) -> Result<PreparedCommand> {
    let script = format!(
        "@echo off\r\necho {}\r\n{}\r\nset \"__TIANGONG_RC_VALUE=%errorlevel%\"\r\necho {}\r\necho %cd%\r\necho {}%__TIANGONG_RC_VALUE%\r\necho {}\r\n",
        markers.start, command, markers.cwd, markers.exit_code, markers.end,
    );
    let mut file = tempfile::Builder::new()
        .prefix(&markers.start)
        .suffix(".cmd")
        .tempfile()
        .context("创建 Windows 终端命令临时文件失败")?;
    file.write_all(script.as_bytes())
        .context("写入 Windows 终端命令临时文件失败")?;
    file.flush().context("刷新 Windows 终端命令临时文件失败")?;
    let path = file.path().to_string_lossy();
    Ok(PreparedCommand {
        input: format!("echo {}>nul & call {}\r", markers.start, shell_quote(&path)),
        _file: Some(file),
    })
}

fn parse_command_output(raw: &[u8], markers: &CommandMarkers) -> ParsedCommandOutput {
    let mut processor = persist::TerminalLineProcessor::new();
    let mut lines = processor.process(&String::from_utf8_lossy(raw));
    let current = processor.current_line();
    if !current.trim().is_empty() {
        lines.push(current);
    }

    let mut start_seen = false;
    let mut collecting_cwd = false;
    let mut output = Vec::new();
    let mut parsed = ParsedCommandOutput::default();
    for line in lines {
        let trimmed = line.trim();
        if trimmed == markers.start {
            start_seen = true;
            continue;
        }
        if trimmed == markers.cwd {
            parsed.cwd_after.clear();
            collecting_cwd = true;
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(&markers.cwd) {
            parsed.cwd_after = value.trim().to_string();
            collecting_cwd = true;
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(&markers.exit_code) {
            collecting_cwd = false;
            parsed.exit_code = value.trim().parse().ok();
            continue;
        }
        if trimmed == markers.end {
            parsed.completed = start_seen;
            break;
        }
        if collecting_cwd {
            parsed.cwd_after.push_str(trimmed);
            continue;
        }
        if start_seen && !contains_marker(trimmed) {
            output.push(line.trim_end().to_string());
        }
    }
    parsed.stdout = output.join("\n");
    parsed
}

// ── Agent 工具入口（宿主直连；每会话唯一调度者在本进程内）─────────────
//
// 工具编排（选终端、新建、开标签、执行、收结果）全部在 sidecar 内完成：
// 会话归属与权威工作目录来自请求帧携带的宿主上下文（Session.cwd 真相源），
// 不接受插件页面上下文；terminal_id 归属 scope 校验在操作内完成。

/// 工具调用的宿主权威会话上下文（请求帧 context，缺省拒绝执行）。
struct ToolScope {
    session_id: String,
    workspace: String,
    invocation_id: String,
}

/// 活动工具登记守卫：dispatch future 被取消（select 丢弃）时随栈展开
/// 自动移除，与 cancel 钩子的移除幂等互备。
struct ActiveToolGuard {
    active_tools: Arc<Mutex<HashMap<String, String>>>,
    invocation_id: String,
}

impl Drop for ActiveToolGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_tools.lock() {
            active.remove(&self.invocation_id);
        }
    }
}

fn tool_scope(context: Option<&RequestInvocationContext>) -> Result<ToolScope> {
    let context = context.ok_or_else(|| {
        anyhow::anyhow!("终端工具调用缺少宿主会话上下文（session_id 与工作目录由宿主注入）")
    })?;
    Ok(ToolScope {
        session_id: context.session_id.clone(),
        workspace: context.workspace.clone(),
        invocation_id: context.invocation_id.clone(),
    })
}

/// 工具结果（对齐宿主 ToolOutcome 形状）。
#[derive(Debug, Serialize)]
struct ToolOutcome {
    ok: bool,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
    exit_code: i32,
}

impl ToolOutcome {
    fn ok(summary: String) -> Self {
        Self {
            ok: true,
            summary,
            stdout: None,
            stderr: None,
            exit_code: 0,
        }
    }

    fn fail(summary: String) -> Self {
        Self {
            ok: false,
            summary,
            stdout: None,
            stderr: None,
            exit_code: 1,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RunCommandArgs {
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RunShellArgs {
    script: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    interactive: bool,
}

#[derive(Debug, Deserialize)]
struct TerminalSendArgs {
    input: String,
    terminal_id: String,
    #[serde(default)]
    wait: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TerminalCloseArgs {
    terminal_id: String,
}

/// 经进度帧请求宿主执行 App 原语（建立/收起本插件标签）。
async fn request_host_app_open(scope_id: &str, instance_id: &str, focus: bool) {
    let mut payload = serde_json::json!({
        "session_id": scope_id,
        "instance_id": instance_id,
    });
    if !focus {
        // 静默建标签（不弹面板）与 shell 时代 showPanel:false 对齐；
        // 用户明确请求展示的 terminal_open 才展开面板。
        payload["mode"] = serde_json::json!("background");
    }
    let message = serde_json::json!({
        "host_action": {
            "method": "app.open",
            "payload": payload,
        }
    });
    tiangong_plugin_sidecar::emit_progress(message.to_string()).await;
}

async fn request_host_app_close(scope_id: &str, instance_id: &str) {
    let message = serde_json::json!({
        "host_action": {
            "method": "app.close",
            "payload": {
                "session_id": scope_id,
                "instance_id": instance_id,
            },
        }
    });
    tiangong_plugin_sidecar::emit_progress(message.to_string()).await;
}

impl TerminalService {
    /// run_command / run_shell：优先复用当前会话的空闲长期终端，没有则
    /// 新建；终端对用户可见可关，实例编号与 PTY 一致，幂等聚焦同一标签。
    async fn tool_run_command(
        &self,
        scope: &ToolScope,
        is_shell: bool,
        payload: serde_json::Value,
    ) -> ToolOutcome {
        // run_command 与 run_shell 共用执行链，仅命令形态不同：前者是
        // cmd + args，后者是 script + interactive。
        let (command, args, cwd_arg, timeout, interactive) = if is_shell {
            match serde_json::from_value::<RunShellArgs>(payload) {
                Ok(parsed) => (
                    parsed.script,
                    Vec::new(),
                    parsed.cwd,
                    parsed.timeout,
                    parsed.interactive,
                ),
                Err(error) => return ToolOutcome::fail(format!("run_shell 参数无效：{error}")),
            }
        } else {
            match serde_json::from_value::<RunCommandArgs>(payload) {
                Ok(parsed) => (parsed.cmd, parsed.args, parsed.cwd, parsed.timeout, false),
                Err(error) => return ToolOutcome::fail(format!("run_command 参数无效：{error}")),
            }
        };

        let acquired = self.acquire_session(AcquireRequest {
            scope_id: scope.session_id.clone(),
        });
        let mut session_id = acquired.session_id;
        let created_new = session_id.is_none();
        // cwd 优先级：Agent 显式指定 > 宿主权威会话工作目录。
        let workspace = cwd_arg
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let workspace = scope.workspace.trim();
                (!workspace.is_empty()).then(|| workspace.to_string())
            });
        if session_id.is_none() {
            let spawned = self.spawn_session(SpawnRequest {
                session_id: None,
                cmd: String::new(),
                args: Vec::new(),
                script: None,
                cwd: workspace.clone(),
                scope_id: Some(scope.session_id.clone()),
                reserve: true,
                cols: default_cols(),
                rows: default_rows(),
            });
            match spawned {
                Ok(spawned) => session_id = Some(spawned.session_id),
                Err(error) => return ToolOutcome::fail(format!("终端会话创建失败：{error:#}")),
            }
        }
        let session_id = match session_id {
            Some(session_id) => session_id,
            None => return ToolOutcome::fail("终端会话创建失败".to_string()),
        };

        // 命令执行静默建立前台标签（不弹面板）：终端对用户可见可关，
        // 重复调用幂等聚焦同一标签。经进度帧即时请求，长任务执行期间
        // 用户即可看到输出。
        request_host_app_open(&scope.session_id, &session_id, false).await;

        // 登记执行中的调用：宿主超时/会话取消经 Cancel 帧中断等待时，
        // cancel 钩子据此定位终端并发送 Ctrl+C，命令不会脱离调用方控制。
        let _active = self.track_active_tool(&scope.invocation_id, &session_id);

        let executed = self
            .exec_in_session(ExecRequest {
                session_id: session_id.clone(),
                cmd: if is_shell {
                    String::new()
                } else {
                    command.clone()
                },
                args: if is_shell { Vec::new() } else { args.clone() },
                script: if is_shell {
                    Some(command.clone())
                } else {
                    None
                },
                timeout,
                interactive,
                cwd: workspace.filter(|value| !value.is_empty()),
            })
            .await;
        drop(_active);
        let executed = match executed {
            Ok(executed) => executed,
            Err(error) => {
                // 执行失败时释放预留（幂等；会话已退出则忽略）。
                let _ = self.release_session(SessionIdRequest {
                    session_id: session_id.clone(),
                });
                return ToolOutcome::fail(format!("终端执行失败：{error:#}"));
            }
        };
        let exit_code = executed.exit_code;
        let cwd_note = if executed.cwd_after.is_empty() {
            String::new()
        } else {
            format!("，cwd: {}", executed.cwd_after)
        };
        let reason = if acquired.reason == "all_busy" {
            "当前会话已有终端都在忙"
        } else {
            "当前会话没有可用终端"
        };
        let selection = if created_new {
            format!("新终端 {session_id}（{reason}，没有写入旧终端）")
        } else {
            format!("终端 {session_id}（复用空闲终端）")
        };
        let summary = if executed.interactive_mode {
            format!("命令已在{selection}进入交互状态")
        } else if executed.timed_out {
            format!("命令在{selection}执行超时{cwd_note}")
        } else if exit_code == 0 {
            format!("命令已在{selection}执行完成{cwd_note}")
        } else {
            format!("命令已在{selection}结束，退出码 {exit_code}{cwd_note}")
        };
        ToolOutcome {
            ok: true,
            summary,
            stdout: Some(executed.stdout),
            stderr: Some(executed.stderr),
            exit_code,
        }
    }

    /// terminal_open：创建独立编号的终端并展开面板聚焦（用户明确请求
    /// 展示时使用）。
    async fn tool_terminal_open(&self, scope: &ToolScope) -> ToolOutcome {
        let instance_id = format!("terminal-{}", scru128::new());
        let spawned = self.spawn_session(SpawnRequest {
            session_id: Some(instance_id.clone()),
            cmd: String::new(),
            args: Vec::new(),
            script: None,
            cwd: (!scope.workspace.trim().is_empty()).then(|| scope.workspace.trim().to_string()),
            scope_id: Some(scope.session_id.clone()),
            reserve: false,
            cols: default_cols(),
            rows: default_rows(),
        });
        if let Err(error) = spawned {
            return ToolOutcome::fail(format!("终端面板操作失败：{error:#}"));
        }
        request_host_app_open(&scope.session_id, &instance_id, true).await;
        ToolOutcome::ok(format!("已打开终端 {instance_id}"))
    }

    /// terminal_send：向指定终端发送输入；目标必须属于当前会话。
    async fn tool_terminal_send(
        &self,
        scope: &ToolScope,
        payload: serde_json::Value,
    ) -> ToolOutcome {
        let parsed = match serde_json::from_value::<TerminalSendArgs>(payload) {
            Ok(parsed) => parsed,
            Err(error) => return ToolOutcome::fail(format!("terminal_send 参数无效：{error}")),
        };
        let target = parsed.terminal_id.trim().to_string();
        if target.is_empty() {
            return ToolOutcome::fail("terminal_send 缺少 terminal_id".to_string());
        }
        let sent = self
            .send_to_session(SendRequest {
                scope_id: scope.session_id.clone(),
                session_id: target,
                input: parsed.input,
                wait: parsed.wait,
            })
            .await;
        match sent {
            Ok(sent) => ToolOutcome {
                ok: true,
                summary: format!("终端 {} 已更新，当前内容见 stdout", sent.session_id),
                stdout: Some(sent.stdout),
                stderr: None,
                exit_code: sent.exit_code,
            },
            Err(error) => ToolOutcome::fail(format!("{error:#}")),
        }
    }

    /// terminal_close：精确关闭指定终端及其面板（归属校验在关闭操作内）。
    async fn tool_terminal_close(
        &self,
        scope: &ToolScope,
        payload: serde_json::Value,
    ) -> ToolOutcome {
        let parsed = match serde_json::from_value::<TerminalCloseArgs>(payload) {
            Ok(parsed) => parsed,
            Err(error) => return ToolOutcome::fail(format!("terminal_close 参数无效：{error}")),
        };
        let target = parsed.terminal_id.trim().to_string();
        if target.is_empty() {
            return ToolOutcome::fail(
                "terminal_close 缺少 terminal_id，未关闭任何终端".to_string(),
            );
        }
        // App 标签可能先于 PTY 退出被用户关闭，关闭操作幂等；但归属
        // 校验失败（终端属于其他会话）必须明确拒绝，不回退当前可见会话。
        match self.close_session(CloseRequest {
            session_id: Some(target.clone()),
            scope_id: scope.session_id.clone(),
        }) {
            Ok(_) => {}
            Err(error) => return ToolOutcome::fail(format!("终端面板操作失败：{error:#}")),
        }
        // 标签已不在前台时仍保证收起；操作幂等。
        request_host_app_close(&scope.session_id, &target).await;
        ToolOutcome::ok(format!("已关闭终端 {target}"))
    }
}

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for TerminalService {
    async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "terminal 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                    request.protocol_version
                ),
                false,
            );
        }
        if request.operation != HANDSHAKE_OPERATION {
            self.mark_activity();
        }
        let payload = match dispatch_operation(
            self,
            &request.operation,
            request.payload.clone(),
            request.context.as_ref(),
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                return Response::error(
                    &request_id,
                    ErrorCode::ServiceError,
                    format!("{error:#}"),
                    false,
                );
            }
        };
        Response::success(&request_id, payload)
    }

    /// 请求级取消：宿主工具超时或会话取消经 Cancel 帧到达（stdio runner
    /// 已丢弃等待 future），把取消落到具体 PTY——发送 Ctrl+C 中断正在
    /// 执行的命令并复位执行状态，命令不会脱离调用方无限运行。
    async fn cancel(&self, request: &Request) -> Result<()> {
        let Some(context) = request.context.as_ref() else {
            return Ok(());
        };
        let terminal_id = self
            .active_tools
            .lock()
            .expect("活动工具表锁损坏")
            .remove(&context.invocation_id);
        let Some(terminal_id) = terminal_id else {
            // 非执行型调用（send 的画面等待等）：等待 future 已被 runner
            // 丢弃，无 PTY 资源需要释放。
            return Ok(());
        };
        let interrupted = self.with_session(&terminal_id, |session| {
            session
                .writer
                .write_all(b"\x03")
                .context("取消写入中断输入失败")?;
            session.writer.flush().context("取消刷新中断输入失败")?;
            // 等待 future 已被丢弃，phase 恢复逻辑不会执行；此处复位，
            // 终端回到空闲池可被后续调用复用。
            session.phase = SessionPhase::Idle;
            Ok(())
        });
        match interrupted {
            Ok(()) => tracing::info!(
                invocation_id = %context.invocation_id,
                terminal = %terminal_id,
                "已按取消请求中断终端命令"
            ),
            Err(error) => {
                // 终端已退出（会话结束）等场景：无需中断，登记已清除。
                tracing::debug!(
                    invocation_id = %context.invocation_id,
                    terminal = %terminal_id,
                    %error,
                    "取消时终端已不可写"
                );
            }
        }
        Ok(())
    }
}

async fn dispatch_operation(
    service: &TerminalService,
    operation: &str,
    payload: serde_json::Value,
    context: Option<&RequestInvocationContext>,
) -> Result<serde_json::Value> {
    // Agent 工具入口（宿主直连）：会话归属来自宿主注入的请求上下文，
    // 缺失即拒绝——不允许脱离会话真相源执行终端操作。
    if matches!(
        operation,
        "run_command" | "run_shell" | "terminal_open" | "terminal_send" | "terminal_close"
    ) {
        let scope = tool_scope(context)?;
        let outcome = match operation {
            "run_command" => service.tool_run_command(&scope, false, payload).await,
            "run_shell" => service.tool_run_command(&scope, true, payload).await,
            "terminal_open" => service.tool_terminal_open(&scope).await,
            "terminal_send" => service.tool_terminal_send(&scope, payload).await,
            _ => service.tool_terminal_close(&scope, payload).await,
        };
        return Ok(serde_json::to_value(outcome)?);
    }
    match operation {
        HANDSHAKE_OPERATION => Ok(serde_json::json!({
            "plugin_id": "terminal",
            "plugin_version": env!("CARGO_PKG_VERSION"),
            "sidecar_version": env!("CARGO_PKG_VERSION"),
            "protocol_version": PROTOCOL_VERSION,
            "business_protocol": 1,
            "instance_id": format!("terminal-sidecar-{}", std::process::id()),
            "status": "ready",
        })),
        "terminalSpawn" => {
            let request: SpawnRequest =
                serde_json::from_value(payload).context("terminalSpawn 参数无效")?;
            Ok(serde_json::to_value(service.spawn_session(request)?)?)
        }
        "terminalAcquire" => {
            let request: AcquireRequest =
                serde_json::from_value(payload).context("terminalAcquire 参数无效")?;
            Ok(serde_json::to_value(service.acquire_session(request))?)
        }
        "terminalRelease" => {
            let request: SessionIdRequest =
                serde_json::from_value(payload).context("terminalRelease 参数无效")?;
            Ok(serde_json::to_value(service.release_session(request)?)?)
        }
        "terminalExec" => {
            let request: ExecRequest =
                serde_json::from_value(payload).context("terminalExec 参数无效")?;
            Ok(serde_json::to_value(
                service.exec_in_session(request).await?,
            )?)
        }
        "terminalWrite" => {
            let request: WriteRequest =
                serde_json::from_value(payload).context("terminalWrite 参数无效")?;
            service.with_session(&request.session_id, |session| {
                session
                    .writer
                    .write_all(request.data.as_bytes())
                    .context("写入 PTY 失败")?;
                session.writer.flush().context("刷新 PTY 失败")
            })?;
            Ok(serde_json::to_value(OkResponse { ok: true })?)
        }
        "terminalSend" => {
            let request: SendRequest =
                serde_json::from_value(payload).context("terminalSend 参数无效")?;
            Ok(serde_json::to_value(
                service.send_to_session(request).await?,
            )?)
        }
        "terminalScreenUpdate" => {
            let request: ScreenUpdateRequest =
                serde_json::from_value(payload).context("terminalScreenUpdate 参数无效")?;
            Ok(serde_json::to_value(service.update_screen(request)?)?)
        }
        "terminalResize" => {
            let request: ResizeRequest =
                serde_json::from_value(payload).context("terminalResize 参数无效")?;
            service.with_session(&request.session_id, |session| {
                session
                    .master
                    .resize(PtySize {
                        rows: request.rows,
                        cols: request.cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .context("调整 PTY 尺寸失败")
            })?;
            Ok(serde_json::to_value(OkResponse { ok: true })?)
        }
        "terminalKill" => {
            let request: SessionIdRequest =
                serde_json::from_value(payload).context("terminalKill 参数无效")?;
            Ok(serde_json::to_value(service.kill_session(request)?)?)
        }
        "terminalClose" => {
            let request: CloseRequest =
                serde_json::from_value(payload).context("terminalClose 参数无效")?;
            Ok(serde_json::to_value(service.close_session(request)?)?)
        }
        // 宿主实例移除兜底：前端标签关闭时若插件页面未就绪（冷启动窗口）
        // 或已卸载，关闭前通知无人接收；宿主在移除标签后对本操作补发一次，
        // 保证终端被回收。幂等（页面已关闭过则无副作用），归属校验同
        // terminalClose——实例编号与终端编号一一对应。
        "instanceClosed" => {
            let request: CloseRequest =
                serde_json::from_value(payload).context("instanceClosed 参数无效")?;
            Ok(serde_json::to_value(service.close_session(request)?)?)
        }
        "terminalFind" => {
            let request: FindRequest =
                serde_json::from_value(payload).context("terminalFind 参数无效")?;
            Ok(serde_json::to_value(service.find_by_scope(&request))?)
        }
        other => bail!("未知操作: {other}"),
    }
}

// PtySystem trait object 供 openpty 使用（NativePtySystem 已是具体类型，此处不需）
#[allow(dead_code)]
fn _pty_system_type_check(_: &dyn PtySystem) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_plugin_sidecar::SidecarService as _;

    #[test]
    fn split_command_拆分含参数的cmd() {
        let (program, args) = split_command("git status --short");
        assert_eq!(program, "git");
        assert_eq!(args, vec!["status".to_string(), "--short".to_string()]);
    }

    #[test]
    fn split_command_引号成组() {
        let (program, args) = split_command("echo 'a b' \"c d\"");
        assert_eq!(program, "echo");
        assert_eq!(args, vec!["a b".to_string(), "c d".to_string()]);
    }

    #[test]
    fn split_command_裸命令无参数() {
        let (program, args) = split_command("ls");
        assert_eq!(program, "ls");
        assert!(args.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn shell_ready_probe_uses_cmd_syntax() {
        assert_eq!(
            shell_ready_probe("__TIANGONG_READY_test__"),
            "echo __TIANGONG_^READY_test__\r"
        );
    }

    #[test]
    fn marker_filter_hides_wrapper_and_keeps_command_output() {
        let mut filter = RawOutputFilter::new();
        let visible = filter.filter(
            "$ __TIANGONG_= echo '__TIANGONG_START_x__'; ls\r\n\
             __TIANGONG_START_x__\r\nCargo.toml\r\n\
             __TIANGONG_RC_x__0\r\n",
        );
        assert_eq!(visible, "Cargo.toml\r\n");
    }

    #[test]
    fn marker_filter_handles_windows_cursor_moves_and_wrapping() {
        let mut filter = RawOutputFilter::new();
        let visible = filter.filter(
            "__TIANGONG_START_x__\x1b[10;1HC:\\work>echo __TIANGONG_STA\x1b[11;80HRT_x__>nul & dir /b\r\nresult.txt\x1b[13;1HC:\\work>set __TIANGONG_RC_VALUE=0\x1b[14;1H",
        );
        assert_eq!(visible, "result.txt\x1b[13;1H");
    }

    #[test]
    fn command_parser_returns_stdout_exit_code_and_cwd() {
        let markers = CommandMarkers::new("x");
        let parsed = parse_command_output(
            b"prompt wrapper\r\n__TIANGONG_START_x__\r\none\r\ntwo\r\n__TIANGONG_CWD_x__/tmp\r\n__TIANGONG_RC_x__7\r\n__TIANGONG_END_x__\r\n",
            &markers,
        );
        assert!(parsed.completed);
        assert_eq!(parsed.stdout, "one\ntwo");
        assert_eq!(parsed.exit_code, Some(7));
        assert_eq!(parsed.cwd_after, "/tmp");
    }

    #[test]
    fn command_parser_joins_wrapped_cwd_without_polluting_stdout() {
        let markers = CommandMarkers::new("x");
        let parsed = parse_command_output(
            b"__TIANGONG_START_x__\r\nagain\r\n__TIANGONG_CWD_x__\r\nC:\\Users\\Test\\very-long\r\n                                                                               path\r\n__TIANGONG_RC_x__0\r\n__TIANGONG_END_x__\r\n",
            &markers,
        );
        assert!(parsed.completed);
        assert_eq!(parsed.stdout, "again");
        assert_eq!(parsed.cwd_after, r"C:\Users\Test\very-longpath");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_exec_returns_output_and_keeps_shell_alive() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        std::fs::write(cwd.path().join("terminal-result.txt"), "ok").expect("创建测试文件失败");
        let service = TerminalService::new();
        let spawned = service
            .spawn_session(SpawnRequest {
                session_id: None,
                cmd: String::new(),
                args: Vec::new(),
                script: None,
                cwd: Some(cwd.path().to_string_lossy().to_string()),
                scope_id: Some("terminal-sidecar-test".to_string()),
                reserve: false,
                cols: 80,
                rows: 24,
            })
            .expect("创建测试终端失败");

        let first = service
            .exec_in_session(ExecRequest {
                session_id: spawned.session_id.clone(),
                cmd: "ls".to_string(),
                args: Vec::new(),
                script: None,
                timeout: Some(10),
                interactive: false,
                cwd: None,
            })
            .await
            .expect("第一次命令执行失败");
        let raw = service
            .with_session(&spawned.session_id, |session| {
                Ok(String::from_utf8_lossy(&session.raw_history).to_string())
            })
            .unwrap_or_default();
        assert_eq!(first.exit_code, 0, "第一次命令结果: {first:?}\nraw={raw:?}");
        assert_eq!(first.stdout, "terminal-result.txt");
        assert_eq!(
            first.cwd_after,
            cwd.path()
                .canonicalize()
                .expect("规范化测试目录失败")
                .to_string_lossy()
        );

        let second = service
            .exec_in_session(ExecRequest {
                session_id: spawned.session_id.clone(),
                cmd: String::new(),
                args: Vec::new(),
                script: Some("false".to_string()),
                timeout: Some(10),
                interactive: false,
                cwd: None,
            })
            .await
            .expect("第二次命令执行失败");
        assert_eq!(second.exit_code, 1);

        let timed_out = service
            .exec_in_session(ExecRequest {
                session_id: spawned.session_id.clone(),
                cmd: String::new(),
                args: Vec::new(),
                script: Some("sleep 5".to_string()),
                timeout: Some(1),
                interactive: false,
                cwd: None,
            })
            .await
            .expect("超时命令执行失败");
        assert!(timed_out.timed_out);
        assert_eq!(timed_out.exit_code, -1);

        let after_timeout = service
            .exec_in_session(ExecRequest {
                session_id: spawned.session_id.clone(),
                cmd: String::new(),
                args: Vec::new(),
                script: Some("printf 'still-alive\\n'".to_string()),
                timeout: Some(10),
                interactive: false,
                cwd: None,
            })
            .await
            .expect("超时后的命令执行失败");
        assert_eq!(after_timeout.exit_code, 0);
        assert_eq!(after_timeout.stdout, "still-alive");
        assert!(
            service
                .with_session(&spawned.session_id, |_| Ok(()))
                .is_ok()
        );

        service
            .kill_session(SessionIdRequest {
                session_id: spawned.session_id,
            })
            .expect("清理测试终端失败");
    }

    /// 构造带宿主权威上下文的工具请求（scope 为 None 即缺上下文场景）。
    fn tool_request(
        operation: &str,
        payload: serde_json::Value,
        scope: Option<(&str, &str)>,
    ) -> Request {
        Request {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: format!("test-{}", scru128::new()),
            operation: operation.to_string(),
            payload,
            context: scope.map(|(session_id, workspace)| RequestInvocationContext {
                session_id: session_id.to_string(),
                invocation_id: "test-invocation".to_string(),
                workspace: workspace.to_string(),
            }),
        }
    }

    fn outcome_of(response: Response) -> serde_json::Value {
        assert!(response.success, "工具调用失败: {response:?}");
        response.payload.expect("成功响应必须带 payload")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 工具调用缺少宿主上下文被拒绝() {
        let service = TerminalService::new();
        let response = service
            .dispatch(tool_request(
                "run_command",
                serde_json::json!({"cmd": "ls"}),
                None,
            ))
            .await;
        assert!(!response.success, "缺上下文必须拒绝: {response:?}");
        let message = response.error_message.unwrap_or_default();
        assert!(message.contains("宿主会话上下文"), "拒绝原因: {message}");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_command_建立会话终端_复用空闲终端() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        std::fs::write(cwd.path().join("marker.txt"), "ok").expect("写测试文件失败");
        let service = TerminalService::new();

        let first = outcome_of(
            service
                .dispatch(tool_request(
                    "run_command",
                    serde_json::json!({"cmd": "ls", "args": []}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(first["ok"], true, "首次执行结果: {first}");
        assert_eq!(first["exit_code"], 0);
        assert_eq!(first["stdout"], "marker.txt");
        // 权威 cwd 生效：命令在宿主注入的会话工作目录执行（shell 回显的
        // 目录即传入的工作目录——逻辑路径原样，不做物理化）。
        assert!(
            first["summary"]
                .as_str()
                .unwrap_or_default()
                .contains(&workspace),
            "summary 应体现权威工作目录: {}",
            first["summary"]
        );
        let terminal = first["summary"]
            .as_str()
            .and_then(|summary| {
                summary
                    .split("终端 ")
                    .nth(1)
                    .and_then(|rest| rest.split('（').next())
            })
            .expect("summary 应含终端编号")
            .to_string();
        assert!(!terminal.is_empty());
        assert!(
            terminal.starts_with("tty-"),
            "自动创建的终端编号应带 tty- 前缀: {terminal}"
        );

        let second = outcome_of(
            service
                .dispatch(tool_request(
                    "run_command",
                    serde_json::json!({"cmd": "echo", "args": ["again"]}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(second["ok"], true);
        assert!(
            second["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("复用空闲终端"),
            "第二次应复用空闲终端: {}",
            second["summary"]
        );

        service
            .kill_session(SessionIdRequest {
                session_id: terminal,
            })
            .expect("清理测试终端失败");
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_command_windows_首次执行并复用同一终端() {
        let cwd = tempfile::tempdir().expect("创建 Windows 测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        std::fs::write(cwd.path().join("terminal-ready-windows.txt"), "ok")
            .expect("写 Windows 测试文件失败");
        let service = TerminalService::new();

        let first = outcome_of(
            service
                .dispatch(tool_request(
                    "run_command",
                    serde_json::json!({"cmd": "dir", "args": []}),
                    Some(("windows-session", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(first["ok"], true, "首次执行结果: {first}");
        assert_eq!(first["exit_code"], 0, "首次执行结果: {first}");
        assert!(
            first["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("terminal-ready-windows.txt"),
            "首次执行未返回目录内容: {first}"
        );
        let terminal = first["summary"]
            .as_str()
            .and_then(|summary| {
                summary
                    .split("终端 ")
                    .nth(1)
                    .and_then(|rest| rest.split('（').next())
            })
            .expect("首次执行摘要应含终端编号")
            .to_string();

        let second = outcome_of(
            service
                .dispatch(tool_request(
                    "run_command",
                    serde_json::json!({"cmd": "echo", "args": ["again"]}),
                    Some(("windows-session", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(second["ok"], true, "第二次执行结果: {second}");
        assert_eq!(second["exit_code"], 0, "第二次执行结果: {second}");
        let raw = service
            .with_session(&terminal, |session| {
                Ok(String::from_utf8_lossy(&session.raw_history).to_string())
            })
            .unwrap_or_default();
        assert_eq!(
            second["stdout"], "again",
            "第二次执行结果: {second}\nraw={raw:?}"
        );
        assert!(
            second["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("复用空闲终端"),
            "第二次执行未复用终端: {second}"
        );

        tokio::time::sleep(Duration::from_millis(100)).await;
        let display = service
            .with_session(&terminal, |session| {
                Ok(String::from_utf8_lossy(&session.display_history).to_string())
            })
            .expect("读取 Windows 终端可见历史失败");
        assert!(display.contains("terminal-ready-windows.txt"));
        assert!(
            !contains_marker(&display),
            "可见输出泄露内部标记: {display:?}"
        );
        service
            .kill_session(SessionIdRequest {
                session_id: terminal,
            })
            .expect("清理 Windows 测试终端失败");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_send_同会话输入送达并返回画面() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        let service = TerminalService::new();

        let first = outcome_of(
            service
                .dispatch(tool_request(
                    "run_command",
                    serde_json::json!({"cmd": "echo", "args": ["ready"]}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(first["ok"], true);
        let terminal = first["summary"]
            .as_str()
            .and_then(|summary| {
                summary
                    .split("终端 ")
                    .nth(1)
                    .and_then(|rest| rest.split('（').next())
            })
            .expect("summary 应含终端编号")
            .to_string();

        // 命令结束后 shell 回到提示符：向该终端发送新命令，输入应送达
        // 且返回内容包含命令回显与输出（视图在线走画面快照，离线走
        // 原始输出回显——测试无视图，验证回显路径）。
        let sent = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_send",
                    serde_json::json!({
                        "terminal_id": terminal,
                        "input": "echo sent-ok\r",
                        "wait": 3,
                    }),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(sent["ok"], true, "terminal_send 结果: {sent}");
        assert!(
            sent["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("sent-ok"),
            "回显应包含发送的命令输出: {}",
            sent["stdout"]
        );

        service
            .kill_session(SessionIdRequest {
                session_id: terminal,
            })
            .expect("清理测试终端失败");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_open后的唯一编号可继续交互() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        let service = TerminalService::new();

        let opened = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_open",
                    serde_json::json!({}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(opened["ok"], true, "terminal_open 结果: {opened}");
        let terminal = opened["summary"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("已打开终端 ")
            .to_string();
        assert!(
            terminal.starts_with("terminal-"),
            "terminal_open 应返回独立编号: {terminal}"
        );

        // 空闲终端建立后即可按编号发送输入（用户面板的等价路径）。
        let sent = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_send",
                    serde_json::json!({
                        "terminal_id": terminal,
                        "input": "echo opened\r",
                        "wait": 3,
                    }),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(sent["ok"], true, "send 结果: {sent}");
        assert!(
            sent["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("opened"),
            "回显应包含输出: {}",
            sent["stdout"]
        );

        service
            .kill_session(SessionIdRequest {
                session_id: terminal,
            })
            .expect("清理测试终端失败");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 跨会话操作被明确拒绝() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        let service = TerminalService::new();

        let first = outcome_of(
            service
                .dispatch(tool_request(
                    "run_command",
                    serde_json::json!({"cmd": "sleep", "args": ["30"], "timeout": 60}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        // 长命令超时返回 ok（命令状态），终端保留运行中。
        assert_eq!(first["ok"], true);
        let terminal = first["summary"]
            .as_str()
            .and_then(|summary| {
                summary
                    .split("终端 ")
                    .nth(1)
                    .and_then(|rest| rest.split('（').next())
            })
            .expect("summary 应含终端编号")
            .to_string();

        // 会话 B 不得发送输入或关闭 A 的终端。
        let sent = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_send",
                    serde_json::json!({"terminal_id": terminal, "input": "ls"}),
                    Some(("session-b", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(sent["ok"], false, "跨会话 terminal_send 必须拒绝");
        let closed = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_close",
                    serde_json::json!({"terminal_id": terminal}),
                    Some(("session-b", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(closed["ok"], false, "跨会话 terminal_close 必须拒绝");

        // 归属会话关闭正常，且关闭未知终端幂等成功。
        let closed_by_owner = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_close",
                    serde_json::json!({"terminal_id": terminal}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(closed_by_owner["ok"], true);
        let closed_missing = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_close",
                    serde_json::json!({"terminal_id": "tty-not-exist"}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(closed_missing["ok"], true);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 多会话各自持有独立终端() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        std::fs::write(cwd.path().join("shared.txt"), "ok").expect("写测试文件失败");
        let service = TerminalService::new();

        let (a, b) = tokio::join!(
            service.dispatch(tool_request(
                "run_command",
                serde_json::json!({"cmd": "ls"}),
                Some(("session-a", workspace.as_str())),
            )),
            service.dispatch(tool_request(
                "run_command",
                serde_json::json!({"cmd": "ls"}),
                Some(("session-b", workspace.as_str())),
            )),
        );
        let (a, b) = (outcome_of(a), outcome_of(b));
        assert_eq!(a["ok"], true);
        assert_eq!(b["ok"], true);
        let terminal_of = |value: &serde_json::Value| {
            value["summary"]
                .as_str()
                .and_then(|summary| {
                    summary
                        .split("终端 ")
                        .nth(1)
                        .and_then(|rest| rest.split('（').next())
                })
                .expect("summary 应含终端编号")
                .to_string()
        };
        let (terminal_a, terminal_b) = (terminal_of(&a), terminal_of(&b));
        assert_ne!(terminal_a, terminal_b, "两个会话必须各建独立终端");

        // 关闭 A 的终端不影响 B 的终端继续执行。
        let closed = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_close",
                    serde_json::json!({"terminal_id": terminal_a}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(closed["ok"], true);
        let b_again = outcome_of(
            service
                .dispatch(tool_request(
                    "run_command",
                    serde_json::json!({"cmd": "echo", "args": ["still"]}),
                    Some(("session-b", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(b_again["ok"], true);
        assert!(
            b_again["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("复用空闲终端"),
            "B 的终端不受 A 关闭影响: {}",
            b_again["summary"]
        );

        service
            .kill_session(SessionIdRequest {
                session_id: terminal_b,
            })
            .expect("清理测试终端失败");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_open_返回独立编号终端() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        let service = TerminalService::new();
        let opened = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_open",
                    serde_json::json!({}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(opened["ok"], true, "terminal_open 结果: {opened}");
        let terminal = opened["summary"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("已打开终端 ")
            .to_string();
        assert!(
            terminal.starts_with("terminal-"),
            "terminal_open 应返回独立编号: {terminal}"
        );
        service
            .kill_session(SessionIdRequest {
                session_id: terminal,
            })
            .expect("清理测试终端失败");
    }

    /// 取消链路：宿主超时/会话取消后 Cancel 帧到达，dispatch future 已被
    /// runner 丢弃——cancel 钩子必须把 Ctrl+C 落到执行中的终端并复位
    /// phase，命令不脱离调用方无限运行、终端可被后续调用复用。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 取消中断执行中的命令并复位终端() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        let service = TerminalService::new();

        // 长命令在独立 task 中执行（对齐 runner：dispatch 由任务持有）。
        let service_for_task = std::sync::Arc::new(service);
        let request = tool_request(
            "run_command",
            serde_json::json!({"cmd": "sleep", "args": ["60"]}),
            Some(("session-a", workspace.as_str())),
        );
        let task_service = std::sync::Arc::clone(&service_for_task);
        let task_request = request.clone();
        let running = tokio::spawn(async move {
            task_service.dispatch(task_request).await;
        });
        // 等待命令进入执行（登记出现）。
        let terminal = {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(active) = service_for_task.active_tools.lock() {
                    if let Some((_, terminal)) = active.iter().next() {
                        break terminal.clone();
                    }
                }
                assert!(Instant::now() < deadline, "长命令应登记为活动工具调用");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };

        // 模拟宿主超时：先执行取消钩子（此时等待 future 仍挂起，与
        // stdio runner 的顺序一致），再丢弃等待 future。
        service_for_task
            .cancel(&request)
            .await
            .expect("取消钩子不应失败");
        running.abort();

        // 终端 phase 已复位：后续调用可立即复用该终端并成功执行。
        let after = outcome_of(
            service_for_task
                .dispatch(tool_request(
                    "run_command",
                    serde_json::json!({"cmd": "echo", "args": ["recovered"]}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        assert_eq!(after["ok"], true, "取消后终端应可复用: {after}");
        assert_eq!(after["stdout"], "recovered");
        assert!(
            after["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("复用空闲终端"),
            "取消后应复用被中断的终端: {}",
            after["summary"]
        );
        assert!(
            service_for_task.with_session(&terminal, |_| Ok(())).is_ok(),
            "终端应存活（取消中断命令而非杀死终端）"
        );

        service_for_task
            .kill_session(SessionIdRequest {
                session_id: terminal,
            })
            .expect("清理测试终端失败");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_send_close_缺少terminal_id明确失败() {
        let service = TerminalService::new();
        let scope = Some(("session-a", "/tmp"));
        let sent = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_send",
                    serde_json::json!({"input": "ls"}),
                    scope,
                ))
                .await,
        );
        assert_eq!(sent["ok"], false);
        assert!(
            sent["summary"]
                .as_str()
                .unwrap_or_default()
                .contains("terminal_id")
        );
        let closed = outcome_of(
            service
                .dispatch(tool_request("terminal_close", serde_json::json!({}), scope))
                .await,
        );
        assert_eq!(closed["ok"], false);
    }

    /// 宿主实例移除兜底（instanceClosed）：页面未就绪时由宿主补发；
    /// 幂等（重复/未知编号成功）、跨会话拒绝，与 terminal_close 一致。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn instanceClosed_兜底回收幂等且校验归属() {
        let cwd = tempfile::tempdir().expect("创建测试目录失败");
        let workspace = cwd.path().to_string_lossy().to_string();
        let service = TerminalService::new();

        // 模拟手动新建标签：终端编号由调用方指定（tab 编号）。
        let opened = outcome_of(
            service
                .dispatch(tool_request(
                    "terminal_open",
                    serde_json::json!({}),
                    Some(("session-a", workspace.as_str())),
                ))
                .await,
        );
        let terminal = opened["summary"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("已打开终端 ")
            .to_string();

        // 实例通知不携带工具上下文（宿主生命周期操作）。
        let host_notice = |scope_id: &str| Request {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: format!("test-{}", scru128::new()),
            operation: "instanceClosed".to_string(),
            payload: serde_json::json!({
                "scope_id": scope_id,
                "session_id": terminal,
            }),
            context: None,
        };
        // 跨会话兜底通知：拒绝（终端不属于该会话）。
        let denied = service.dispatch(host_notice("session-b")).await;
        assert!(!denied.success, "跨会话兜底必须拒绝: {denied:?}");

        // 归属会话：回收成功；重复补发与未知编号幂等成功。
        let closed = service.dispatch(host_notice("session-a")).await;
        assert!(closed.success, "归属会话兜底应成功: {closed:?}");
        assert!(
            service.with_session(&terminal, |_| Ok(())).is_err(),
            "终端应已被兜底回收"
        );
        let again = service.dispatch(host_notice("session-a")).await;
        assert!(again.success, "重复兜底应幂等成功");
        let unknown = service
            .dispatch(Request {
                protocol_version: PROTOCOL_VERSION.to_string(),
                request_id: format!("test-{}", scru128::new()),
                operation: "instanceClosed".to_string(),
                payload: serde_json::json!({
                    "scope_id": "session-a",
                    "session_id": "tty-not-exist",
                }),
                context: None,
            })
            .await;
        assert!(unknown.success, "未知编号兜底应幂等成功");
    }
}
