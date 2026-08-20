//! PTY 会话服务：请求-响应操作 + 输出流通知。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use portable_pty::{ChildKiller, CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::{Deserialize, Serialize};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, PROTOCOL_VERSION, Request, Response,
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

pub struct TerminalService {
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
    sequence: Mutex<u64>,
}

struct PtySession {
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// 宿主会话标识：终端跟会话走与恢复的锚点（同 scope 取最新会话）。
    scope_id: Option<String>,
    /// 创建序号：单调递增，同 scope 多会话时分辨最新。
    sequence: u64,
    /// 最近输出缓冲：聚合线程追加，terminalFind 读取。
    history: Vec<u8>,
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
    #[serde(default = "default_cols")]
    cols: u16,
    #[serde(default = "default_rows")]
    rows: u16,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest {
    session_id: String,
    data: String,
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
            sequence: Mutex::new(0),
        }
    }

    fn next_sequence(&self) -> u64 {
        let mut sequence = self.sequence.lock().expect("会话序号锁损坏");
        *sequence += 1;
        *sequence
    }

    fn spawn_session(&self, request: SpawnRequest) -> Result<SpawnResponse> {
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

        let sequence = self.next_sequence();
        let session_id = format!("tty-{sequence}");
        let killer = child.clone_killer();
        // 输出持久化（按 scope 分文件）：应用重启后回填该会话的终端历史
        let logger = request
            .scope_id
            .as_deref()
            .and_then(persist::scope_log_path)
            .and_then(persist::OutputLogger::open)
            .map(Arc::new);

        // 输出管道：阻塞读线程只管搬运；聚合线程首批到达后在
        // OUTPUT_FLUSH_INTERVAL_MS 窗口内合并（避免每字节一帧），窗口结束
        // 立即发送。此前在阻塞读上做攒批，PTY 无后续输出时永远等不到
        // 退出条件，已读输出滞留不发送（终端黑框的直接原因）。
        // 发送同时把本批输出追加进会话历史缓冲（UI 重新附着时重放）。
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("克隆 PTY 读取端失败")?;
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
                        let chunk = buffer[..bytes].to_vec();
                        if chunk_tx.send(chunk).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        // 首批输出随响应返回（见 SpawnResponse::boot_output）：最多等
        // 40ms 收 shell 提示符，收到即止（不等满）。已入历史与落盘，
        // 聚合线程只负责后续输出，不产生重复。
        let boot_output = match chunk_rx.recv_timeout(std::time::Duration::from_millis(40)) {
            Ok(first) => {
                record_output(&boot_sessions, &output_session, &first);
                Some(String::from_utf8_lossy(&first).to_string())
            }
            Err(_) => None,
        };
        std::thread::spawn(move || {
            while let Ok(first) = chunk_rx.recv() {
                let mut pending = first;
                while let Ok(more) = chunk_rx
                    .recv_timeout(std::time::Duration::from_millis(OUTPUT_FLUSH_INTERVAL_MS))
                {
                    pending.extend_from_slice(&more);
                    if pending.len() >= 64 * 1024 {
                        break;
                    }
                }
                record_output(&output_sessions, &output_session, &pending);
                let data = String::from_utf8_lossy(&pending).to_string();
                let payload = serde_json::to_string(&OutputNotification {
                    session_id: &output_session,
                    data: &data,
                })
                .unwrap_or_default();
                emit_notification(CHANNEL_OUTPUT, payload);
            }
        });

        // 等待线程：退出通知 + 会话出表（find 不再命中死会话，
        // write/resize 自然返回「会话不存在」）。
        let exit_session = session_id.clone();
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
            exit_sessions
                .lock()
                .expect("会话表锁损坏")
                .remove(&exit_session);
        });

        let writer = pair.master.take_writer().context("获取 PTY 写入端失败")?;
        // master 保留在会话中（resize 用）；openpty 返回的即 Box<dyn MasterPty + Send>
        let master = pair.master;
        self.sessions.lock().expect("会话表锁损坏").insert(
            session_id.clone(),
            PtySession {
                writer,
                killer,
                master,
                scope_id: request.scope_id,
                sequence,
                history: Vec::new(),
                logger,
            },
        );

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
        let removed = request.session_id.as_deref().and_then(|session_id| {
            self.sessions
                .lock()
                .expect("会话表锁损坏")
                .remove(session_id)
        });
        if let Some(mut session) = removed {
            let _ = session.killer.kill();
        }
        persist::clear_scope_log(&request.scope_id).context("清理终端恢复记录失败")?;
        Ok(OkResponse { ok: true })
    }

    /// 按宿主会话标识找最近创建的活跃终端：UI 跟随会话切换时先恢复
    /// 既有 PTY（含最近输出历史），没有再新建。无存活会话时返回磁盘
    /// 日志尾部（应用重启后回填历史，UI 据此新建 shell 并重放）。
    fn find_by_scope(&self, scope_id: &str) -> FindResponse {
        let sessions = self.sessions.lock().expect("会话表锁损坏");
        if let Some((session_id, session)) = sessions
            .iter()
            .filter(|(_, session)| session.scope_id.as_deref() == Some(scope_id))
            .max_by_key(|(_, session)| session.sequence)
        {
            return FindResponse {
                session_id: Some(session_id.clone()),
                history: String::from_utf8_lossy(&session.history).to_string(),
            };
        }
        drop(sessions);
        let history = persist::scope_log_path(scope_id)
            .map(|path| persist::read_log_tail(&path, persist::LOG_TAIL_LINES))
            .unwrap_or_default()
            .join("\r\n");
        FindResponse {
            session_id: None,
            history,
        }
    }
}

/// 记录会话输出：内存环形缓冲（重放历史）+ 磁盘日志（跨重启回填）。
/// 会话已出表则丢弃；文件写在表锁外进行，不阻塞 write/resize。
fn record_output(
    sessions: &Arc<Mutex<HashMap<String, PtySession>>>,
    session_id: &str,
    bytes: &[u8],
) {
    let logger = {
        let mut sessions = sessions.lock().expect("会话表锁损坏");
        let Some(session) = sessions.get_mut(session_id) else {
            return;
        };
        session.history.extend_from_slice(bytes);
        let overflow = session.history.len().saturating_sub(HISTORY_LIMIT_BYTES);
        if overflow > 0 {
            session.history.drain(..overflow);
        }
        session.logger.clone()
    };
    if let Some(logger) = logger {
        logger.append(&String::from_utf8_lossy(bytes));
    }
}

impl Default for TerminalService {
    fn default() -> Self {
        Self::new()
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
                    "terminal-handler 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                    request.protocol_version
                ),
                false,
            );
        }
        let payload = match dispatch_operation(self, &request.operation, request.payload).await {
            Ok(value) => value,
            Err(error) => {
                return Response::error(
                    &request_id,
                    ErrorCode::ServiceError,
                    error.to_string(),
                    false,
                );
            }
        };
        Response::success(&request_id, payload)
    }
}

async fn dispatch_operation(
    service: &TerminalService,
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    match operation {
        HANDSHAKE_OPERATION => Ok(serde_json::json!({
            "plugin_id": "terminal-handler",
            "plugin_version": env!("CARGO_PKG_VERSION"),
            "sidecar_version": env!("CARGO_PKG_VERSION"),
            "protocol_version": PROTOCOL_VERSION,
            "business_protocol": 1,
            "instance_id": format!("terminal-handler-sidecar-{}", std::process::id()),
            "status": "ready",
        })),
        "terminalSpawn" => {
            let request: SpawnRequest =
                serde_json::from_value(payload).context("terminalSpawn 参数无效")?;
            Ok(serde_json::to_value(service.spawn_session(request)?)?)
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
        "terminalFind" => {
            let request: FindRequest =
                serde_json::from_value(payload).context("terminalFind 参数无效")?;
            Ok(serde_json::to_value(
                service.find_by_scope(&request.scope_id),
            )?)
        }
        other => bail!("未知操作: {other}"),
    }
}

// PtySystem trait object 供 openpty 使用（NativePtySystem 已是具体类型，此处不需）
#[allow(dead_code)]
fn _pty_system_type_check(_: &dyn PtySystem) {}
