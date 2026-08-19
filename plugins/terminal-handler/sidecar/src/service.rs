//! PTY 会话服务：请求-响应操作 + 输出流通知。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use anyhow::{Context as _, Result, bail};
use portable_pty::{ChildKiller, CommandBuilder, NativePtySystem, PtySize, PtySystem};
use serde::{Deserialize, Serialize};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, PROTOCOL_VERSION, Request, Response,
};
use tiangong_plugin_sidecar::server::emit_notification;

/// 通知通道：PTY 输出流（负载 JSON 见 `OutputNotification`）。
pub const CHANNEL_OUTPUT: &str = "terminal.output";
/// 通知通道：会话退出（负载 JSON 见 `ExitNotification`）。
pub const CHANNEL_EXIT: &str = "terminal.exit";

/// PTY 输出节流：读取线程按此间隔批量推送（约 60fps 上限）。
const OUTPUT_FLUSH_INTERVAL_MS: u64 = 16;

pub struct TerminalService {
    sessions: Mutex<HashMap<String, PtySession>>,
    sequence: Mutex<u64>,
}

struct PtySession {
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
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

fn default_cols() -> u16 {    80
}
fn default_rows() -> u16 {
    24
}

#[derive(Debug, Serialize)]
struct SpawnResponse {
    session_id: String,
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

#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

impl TerminalService {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            sequence: Mutex::new(0),
        }
    }

    fn next_session_id(&self) -> String {
        let mut sequence = self.sequence.lock().expect("会话序号锁损坏");
        *sequence += 1;
        format!("tty-{sequence}")
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
        if let Some(cwd) = request.cwd.as_deref() {
            command.cwd(cwd);
        }

        let mut child = pair
            .slave
            .spawn_command(command)
            .context("启动 PTY 子进程失败")?;
        drop(pair.slave);

        let session_id = self.next_session_id();
        let killer = child.clone_killer();

        // 输出管道：阻塞读线程只管搬运；聚合线程首批到达后在
        // OUTPUT_FLUSH_INTERVAL_MS 窗口内合并（避免每字节一帧），窗口结束
        // 立即发送。此前在阻塞读上做攒批，PTY 无后续输出时永远等不到
        // 退出条件，已读输出滞留不发送（终端黑框的直接原因）。
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("克隆 PTY 读取端失败")?;
        let output_session = session_id.clone();
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
        std::thread::spawn(move || {
            while let Ok(first) = chunk_rx.recv() {
                let mut pending = first;
                while let Ok(more) =
                    chunk_rx.recv_timeout(std::time::Duration::from_millis(OUTPUT_FLUSH_INTERVAL_MS))
                {
                    pending.extend_from_slice(&more);
                    if pending.len() >= 64 * 1024 {
                        break;
                    }
                }
                let data = String::from_utf8_lossy(&pending).to_string();
                let payload = serde_json::to_string(&OutputNotification {
                    session_id: &output_session,
                    data: &data,
                })
                .unwrap_or_default();
                emit_notification(CHANNEL_OUTPUT, payload);
            }
        });

        // 等待线程：退出通知
        let exit_session = session_id.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            let exit_code = status.as_ref().ok().map(|status| status.exit_code() as u32);
            let payload = serde_json::to_string(&ExitNotification {
                session_id: &exit_session,
                exit_code,
            })
            .unwrap_or_default();
            emit_notification(CHANNEL_EXIT, payload);
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
            },
        );

        Ok(SpawnResponse { session_id })
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
        other => bail!("未知操作: {other}"),
    }
}

// PtySystem trait object 供 openpty 使用（NativePtySystem 已是具体类型，此处不需）
#[allow(dead_code)]
fn _pty_system_type_check(_: &dyn PtySystem) {}
