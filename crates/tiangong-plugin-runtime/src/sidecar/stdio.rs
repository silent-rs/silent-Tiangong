//! stdio 传输的宿主侧连接器（RFC 0017 D16 / S2）。
//!
//! 帧协议与 TCP 完全一致（JSON Lines + Auth 首帧 + Request/Response/Progress/
//! Notification），仅传输通道不同：spawn 时以继承管道直连子进程。
//! 由此沙箱内可零网络放行、无监听端口；Auth token 由宿主生成、经
//! `TIANGONG_PLUGIN_STDIO_TOKEN` 注入子进程，首帧校验。
//!
//! 生命周期与 TCP 版（detached + endpoint 换代重启）不同：stdio 模式下
//! 父子进程强绑定，宿主退出即管道关闭；sidecar 崩溃由下次 invoke 检测并
//! 自动重启（换代）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::protocol::{
    HANDSHAKE_OPERATION, HandshakeResponse, IpcAuth, IpcFrame, IpcRequest, PROTOCOL_VERSION,
    Request, Response,
};
use crate::sidecar::{
    EXEC_ENV_JSON_ENV, PLUGIN_DATA_DIR_ENV, PLUGIN_ENDPOINT_ENV, PLUGIN_ID_ENV, PLUGIN_VERSION_ENV,
    STORAGE_ROOT_ENV, SidecarConfig, SidecarConnection, SidecarInvokeError,
};

/// 子进程环境：传输模式标记。
pub const TRANSPORT_ENV: &str = "TIANGONG_PLUGIN_TRANSPORT";
/// 子进程环境：stdio 模式的认证 token。
pub const STDIO_TOKEN_ENV: &str = "TIANGONG_PLUGIN_STDIO_TOKEN";
pub const TRANSPORT_STDIO: &str = "stdio";

/// stdio 传输连接：单子进程 + 常驻读线程按 request_id 路由响应与进度，
/// Notification 帧走全局通知转发（与 TCP 通知监听等价）。
pub struct StdioSidecarConnection {
    config: SidecarConfig,
    state: Mutex<StdioState>,
    exec_env: Mutex<std::collections::BTreeMap<String, String>>,
}

struct StdioState {
    process: Option<Arc<StdioProcess>>,
}

struct StdioProcess {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<String, PendingWaiter>>>,
    /// 本进程代次的认证 token（Auth 首帧内容，子进程经环境变量持有同值）。
    token: String,
    /// 本进程是否已发送过 Auth 首帧。
    authenticated: AtomicBool,
}

#[derive(Clone)]
struct PendingWaiter {
    response: SyncSender<Result<Value, String>>,
    progress: SyncSender<String>,
}

impl StdioSidecarConnection {
    pub fn new(config: SidecarConfig) -> Self {
        Self {
            config,
            state: Mutex::new(StdioState { process: None }),
            exec_env: Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    /// 当前 sidecar 的插件 ID。
    pub fn plugin_id(&self) -> &str {
        &self.config.plugin_id
    }

    /// 更新 exec_env（下次 spawn 时注入）。
    pub fn update_exec_env(&self, env: std::collections::BTreeMap<String, String>) {
        if let Ok(mut guard) = self.exec_env.lock() {
            *guard = env;
        }
    }

    /// 停止子进程（宿主关闭流程调用）。
    pub fn stop(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("stdio sidecar 状态锁已损坏"))?;
        if let Some(process) = state.process.take()
            && let Ok(mut child) = process.child.lock()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    /// 确保进程存活并完成握手（安装验证 / trait ensure_running 用）。
    pub fn ensure_running_checked(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("stdio sidecar 状态锁已损坏"))?;
        self.ensure_running(&mut state).map(|_| ())
    }

    /// 确保子进程存活且完成过握手。进程退出时自动重启（换代）。
    fn ensure_running(&self, state: &mut StdioState) -> Result<Arc<StdioProcess>> {
        if let Some(process) = state.process.as_ref() {
            let alive = process
                .child
                .lock()
                .map(|mut child| {
                    child
                        .try_wait()
                        .map(|status| status.is_none())
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if alive {
                return Ok(Arc::clone(process));
            }
            tracing::warn!(
                plugin_id = %self.config.plugin_id,
                "stdio sidecar 已退出，准备重启"
            );
            state.process = None;
        }
        let process = Arc::new(self.spawn()?);
        state.process = Some(Arc::clone(&process));
        self.handshake(&process)?;
        tracing::info!(
            plugin_id = %self.config.plugin_id,
            "stdio sidecar 已就绪"
        );
        Ok(process)
    }

    fn spawn(&self) -> Result<StdioProcess> {
        if !self.config.binary.is_file() {
            bail!("sidecar 二进制不存在: {}", self.config.binary.display());
        }
        if let Some(parent) = self.config.log.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建 sidecar 日志目录失败: {}", parent.display()))?;
        }
        std::fs::create_dir_all(&self.config.data_dir).with_context(|| {
            format!(
                "创建 sidecar 数据目录失败: {}",
                self.config.data_dir.display()
            )
        })?;
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.log)
            .with_context(|| format!("打开 sidecar 日志失败: {}", self.config.log.display()))?;

        let token = scru128::new().to_string();
        // OS 沙箱（RFC 0017 D12 继承式）：可写根 = 插件数据目录；宿主数据目录与
        // 工作区 .git 经防篡改段强制只读；沙箱不可用时降级直跑并告警（快照层兜底）。
        let sandboxed_argv = if self.config.sandbox {
            let workspace = self
                .config
                .sandbox_workspace
                .clone()
                .unwrap_or_else(|| self.config.data_dir.clone());
            let mut policy = tiangong_sandbox::SandboxPolicy::workspace_write(workspace);
            policy.allow_network = self.config.sandbox_network;
            match tiangong_sandbox::wrap(&policy) {
                tiangong_sandbox::SandboxedProgram::Wrapped { program, prefix } => {
                    tracing::info!(
                        plugin_id = %self.config.plugin_id,
                        "stdio sidecar 已包装进 OS 沙箱"
                    );
                    Some((program, prefix))
                }
                tiangong_sandbox::SandboxedProgram::Direct => None,
                // 声明了沙箱但平台不可用：拒绝启动（fail loud）。
                tiangong_sandbox::SandboxedProgram::Unavailable(reason) => {
                    bail!(
                        "插件 {} 声明 sidecar.sandbox 但当前平台沙箱不可用：{reason}",
                        self.config.plugin_id
                    );
                }
            }
        } else {
            None
        };
        // 包装形态：<wrapper> <prefix...> <binary>：seatbelt 前缀为
        // `-p <profile>`，bwrap 前缀末尾自带 `--`，均要求被包装命令在最后。
        let mut command = match &sandboxed_argv {
            Some((program, prefix)) => {
                let mut command = Command::new(program);
                for arg in prefix {
                    command.arg(arg);
                }
                command.arg(&self.config.binary);
                command
            }
            None => Command::new(&self.config.binary),
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .env(TRANSPORT_ENV, TRANSPORT_STDIO)
            .env(STDIO_TOKEN_ENV, &token)
            .env(PLUGIN_ID_ENV, &self.config.plugin_id)
            .env(PLUGIN_VERSION_ENV, &self.config.plugin_version)
            // stdio 模式无 endpoint 文件；保留路径占位以兼容读取方。
            .env(PLUGIN_ENDPOINT_ENV, &self.config.endpoint)
            .env(PLUGIN_DATA_DIR_ENV, &self.config.data_dir);
        if self.config.allow_sensitive_storage {
            command.env(STORAGE_ROOT_ENV, &self.config.storage_root);
        }
        if let Some(env) = self.exec_env.lock().ok().filter(|env| !env.is_empty())
            && let Ok(json) = serde_json::to_string(&*env)
        {
            command.env(EXEC_ENV_JSON_ENV, json);
        }
        let mut child = command.spawn().with_context(|| {
            format!("启动 stdio sidecar 失败: {}", self.config.binary.display())
        })?;
        let stdin = child.stdin.take().context("stdio sidecar 未提供 stdin")?;
        let stdout = child.stdout.take().context("stdio sidecar 未提供 stdout")?;
        let pid = child.id();
        tracing::info!(
            plugin_id = %self.config.plugin_id,
            pid,
            transport = TRANSPORT_STDIO,
            "stdio sidecar 已启动"
        );

        let pending: Arc<Mutex<HashMap<String, PendingWaiter>>> =
            Arc::new(Mutex::new(HashMap::new()));
        spawn_stdio_reader(self.config.plugin_id.clone(), stdout, Arc::clone(&pending));
        Ok(StdioProcess {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending,
            token,
            authenticated: AtomicBool::new(false),
        })
    }

    /// 握手校验身份（plugin_id / 协议版本），对齐 TCP health_check。
    fn handshake(&self, process: &Arc<StdioProcess>) -> Result<()> {
        let payload = self.round_trip(
            process,
            HANDSHAKE_OPERATION,
            serde_json::Value::Null,
            &mut |_| {},
        )?;
        let handshake: HandshakeResponse =
            serde_json::from_value(payload).context("解析 stdio sidecar 握手响应失败")?;
        if handshake.plugin_id != self.config.plugin_id {
            bail!(
                "stdio sidecar 插件身份不匹配: expected={}, actual={}",
                self.config.plugin_id,
                handshake.plugin_id
            );
        }
        if handshake.protocol_version != PROTOCOL_VERSION {
            return Err(SidecarInvokeError::ProtocolMismatch(format!(
                "stdio sidecar 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                handshake.protocol_version
            ))
            .into());
        }
        Ok(())
    }

    /// 单请求往返：注册 pending → 写帧（新进程首帧前补 Auth）→ 循环收进度/响应。
    fn round_trip(
        &self,
        process: &Arc<StdioProcess>,
        operation: &str,
        payload: Value,
        on_progress: &mut dyn FnMut(String),
    ) -> Result<Value> {
        let request = Request::new(operation, payload);
        let request_id = request.request_id.clone();
        let (response_tx, response_rx) = sync_channel::<Result<Value, String>>(1);
        let (progress_tx, progress_rx) = sync_channel::<String>(64);
        process
            .pending
            .lock()
            .map_err(|_| anyhow!("stdio sidecar pending 锁已损坏"))?
            .insert(
                request_id.clone(),
                PendingWaiter {
                    response: response_tx,
                    progress: progress_tx,
                },
            );

        let write_result = self.write_request(process, &request);
        if let Err(error) = write_result {
            remove_pending(process, &request_id);
            return Err(SidecarInvokeError::Unavailable(error.to_string()).into());
        }

        let deadline = Instant::now() + self.config.request_timeout;
        loop {
            // 先排空进度，再等响应。
            while let Ok(message) = progress_rx.try_recv() {
                on_progress(message);
            }
            let now = Instant::now();
            let remain = if deadline > now {
                deadline - now
            } else {
                Duration::ZERO
            };
            if remain.is_zero() {
                remove_pending(process, &request_id);
                return Err(SidecarInvokeError::Timeout)
                    .with_context(|| format!("等待 stdio sidecar 响应超时：{operation}"));
            }
            match response_rx.recv_timeout(remain.min(Duration::from_millis(200))) {
                Ok(result) => {
                    return result.map_err(|message| SidecarInvokeError::Internal(message).into());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // 读线程已退出（进程死亡）：按不可用处理，下次 ensure 重启。
                    remove_pending(process, &request_id);
                    return Err(SidecarInvokeError::Unavailable(
                        "stdio sidecar 读通道已关闭".to_string(),
                    )
                    .into());
                }
            }
        }
    }

    /// 写请求帧（每个新进程首个请求前补 Auth 首帧，token 与子进程环境一致）。
    fn write_request(&self, process: &StdioProcess, request: &Request) -> Result<()> {
        let mut stdin = process
            .stdin
            .lock()
            .map_err(|_| anyhow!("stdio sidecar 写端锁已损坏"))?;
        if !process.authenticated.load(Ordering::Acquire) {
            let frame = IpcFrame::Auth(IpcAuth {
                token: process.token.clone(),
            });
            write_line(&mut stdin, &frame)?;
            process.authenticated.store(true, Ordering::Release);
        }
        let frame = IpcFrame::Request(IpcRequest {
            request_id: request.request_id.clone(),
            payload: serde_json::to_value(request).context("序列化 sidecar 请求失败")?,
        });
        write_line(&mut stdin, &frame)
    }
}

fn write_line(stdin: &mut ChildStdin, frame: &IpcFrame) -> Result<()> {
    let line = serde_json::to_string(frame).context("序列化 sidecar 帧失败")?;
    stdin
        .write_all(line.as_bytes())
        .and_then(|_| stdin.write_all(b"\n"))
        .and_then(|_| stdin.flush())
        .context("写入 stdio sidecar 帧失败")
}

fn remove_pending(process: &StdioProcess, request_id: &str) {
    if let Ok(mut pending) = process.pending.lock() {
        pending.remove(request_id);
    }
}

/// 常驻读线程：解析 stdout 的 JSON Lines 帧，按 request_id 路由响应与进度，
/// Notification 帧经全局转发器送出（与 TCP 通知监听等价）。
fn spawn_stdio_reader(
    plugin_id: String,
    stdout: std::process::ChildStdout,
    pending: Arc<Mutex<HashMap<String, PendingWaiter>>>,
) {
    std::thread::Builder::new()
        .name(format!("plugin-sidecar-stdio-reader-{plugin_id}"))
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break, // EOF / IO 错误：进程关闭。
                    Ok(_) => {}
                }
                let Ok(frame) = serde_json::from_str::<IpcFrame>(line.trim_end()) else {
                    tracing::warn!(plugin_id = %plugin_id, "stdio sidecar 输出无法解析的帧");
                    continue;
                };
                match frame {
                    IpcFrame::Response(response) => {
                        if let Some(waiter) = pending
                            .lock()
                            .ok()
                            .and_then(|mut map| map.remove(&response.request_id))
                        {
                            let _ = waiter
                                .response
                                .send(parse_response_payload(response.payload));
                        }
                    }
                    IpcFrame::Progress {
                        request_id,
                        message,
                    } => {
                        if let Some(waiter) = pending
                            .lock()
                            .ok()
                            .and_then(|map| map.get(&request_id).cloned())
                        {
                            let _ = waiter.progress.try_send(message);
                        }
                    }
                    IpcFrame::Notification { channel, payload } => {
                        if let Some(forwarder) = crate::sidecar::sidecar_notification_forwarder() {
                            forwarder(&plugin_id, &channel, &payload);
                        }
                    }
                    IpcFrame::Error { message } => {
                        fail_all_pending(&pending, format!("stdio sidecar 错误: {message}"));
                    }
                    IpcFrame::Auth(_) | IpcFrame::Request(_) => {
                        tracing::warn!(
                            plugin_id = %plugin_id,
                            "stdio sidecar 发送了非预期的帧类型"
                        );
                    }
                }
            }
            fail_all_pending(&pending, "stdio sidecar 已关闭".to_string());
        })
        .expect("启动 stdio 读线程失败");
}

fn fail_all_pending(pending: &Mutex<HashMap<String, PendingWaiter>>, message: String) {
    if let Ok(mut map) = pending.lock() {
        for (_, waiter) in map.drain() {
            let _ = waiter.response.send(Err(message.clone()));
        }
    }
}

/// 解析 Response 信封：协议版本校验 + success 展开（对齐 TCP 实现）。
fn parse_response_payload(payload: Value) -> Result<Value, String> {
    let response: Response = serde_json::from_value(payload)
        .map_err(|error| format!("解析 sidecar 协议响应失败: {error}"))?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "sidecar 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
            response.protocol_version
        ));
    }
    if !response.success {
        return Err(response
            .error_message
            .unwrap_or_else(|| "sidecar 请求失败".to_string()));
    }
    Ok(response.payload.unwrap_or(Value::Null))
}

impl SidecarConnection for StdioSidecarConnection {
    fn invoke(&self, operation: &str, payload: &str) -> Result<String> {
        self.invoke_with_progress(operation, payload, &mut |_| {})
    }

    fn invoke_with_progress(
        &self,
        operation: &str,
        payload: &str,
        on_progress: &mut dyn FnMut(String),
    ) -> Result<String> {
        let payload = serde_json::from_str(payload).with_context(|| "sidecar 请求不是有效 JSON")?;
        let mut state = self.state.lock().map_err(|_| {
            anyhow!(SidecarInvokeError::Unavailable(
                "stdio sidecar 状态锁已损坏".to_string()
            ))
        })?;
        let process = self.ensure_running(&mut state).map_err(|error| {
            if error.downcast_ref::<SidecarInvokeError>().is_some() {
                error
            } else {
                SidecarInvokeError::Unavailable(error.to_string()).into()
            }
        })?;
        let response = self
            .round_trip(&process, operation, payload, on_progress)
            .map_err(|error| {
                if error.downcast_ref::<SidecarInvokeError>().is_some() {
                    error
                } else {
                    SidecarInvokeError::Internal(error.to_string()).into()
                }
            })?;
        serde_json::to_string(&response).with_context(|| "序列化 sidecar 响应失败")
    }

    fn update_exec_env(&self, env: std::collections::BTreeMap<String, String>) {
        StdioSidecarConnection::update_exec_env(self, env);
    }

    fn stop(&self) -> Result<()> {
        StdioSidecarConnection::stop(self)
    }

    fn ensure_running(&self) -> Result<()> {
        StdioSidecarConnection::ensure_running_checked(self)
    }

    fn has_runtime_endpoint(&self) -> bool {
        self.state
            .lock()
            .ok()
            .and_then(|state| {
                state.process.as_ref().map(|process| {
                    process
                        .child
                        .lock()
                        .map(|mut child| {
                            child
                                .try_wait()
                                .map(|status| status.is_none())
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }

    fn plugin_id(&self) -> &str {
        StdioSidecarConnection::plugin_id(self)
    }
}
