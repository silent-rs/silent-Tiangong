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
        // OS 沙箱（RFC 0017 官方沙箱程序）：声明沙箱的 sidecar 一律经
        // tiangong-sandbox 可执行文件启动——策略经 fd3 继承管道传入
        // （双版本化），沙箱程序校验并应用平台沙箱后 exec 目标进程；
        // stdin/stdout 业务通信透传。不可用即 fail-closed（拒绝启动）。
        let launch_policy = if self.config.sandbox {
            let workspace = self
                .config
                .sandbox_workspace
                .clone()
                .unwrap_or_else(|| self.config.data_dir.clone());
            let mut policy = tiangong_sandbox::SandboxPolicy::workspace_write(workspace);
            policy.allow_network = self.config.sandbox_network;
            Some(policy)
        } else {
            None
        };
        // fd 守卫必须存活到 spawn 完成：match 臂内绑定会在臂结束时提前
        // drop、导致沙箱程序读不到策略（审查修复）。
        let mut policy_fd_guard = None;
        let mut command = match &launch_policy {
            Some(policy) => {
                let sandbox_bin =
                    tiangong_sandbox::launcher_manager::resolve_sandbox_binary(
                        &self.config.storage_root,
                    )
                    .ok_or_else(|| {
                        anyhow!(
                            "插件 {} 声明沙箱但 tiangong-sandbox 程序不可用（active/内置均缺失），拒绝启动",
                            self.config.plugin_id
                        )
                    })?;
                let request = serde_json::json!({
                    "protocol_version": 1,
                    "policy_schema": 1,
                    "policy": policy,
                    "program": self.config.binary.display().to_string(),
                    "args": [],
                });
                let mut command = Command::new(sandbox_bin);
                policy_fd_guard = Some(prepare_policy_fd(&mut command, request.to_string())?);
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

/// 为沙箱程序准备策略描述符：匿名管道写端写入策略后立即关闭（读取到
/// EOF 即策略完整）；读端经 pre_exec 复制到 fd3 并关闭原描述符。
///
/// 返回的读端守卫必须存活到 `spawn` 返回——父进程随后正常关闭（无泄漏）；
/// 读端在父进程侧设置 FD_CLOEXEC，避免被无关子进程继承。
struct PolicyFdGuard(std::os::fd::OwnedFd);

fn prepare_policy_fd(command: &mut Command, policy_json: String) -> Result<PolicyFdGuard> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let (read_fd, write_fd) = {
        let mut fds = [0i32; 2];
        // SAFETY: fds 为有效出参缓冲。
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("创建策略管道失败");
        }
        (fds[0], fds[1])
    };
    {
        use std::io::Write;
        let mut writer = unsafe { std::fs::File::from_raw_fd(write_fd) };
        writer
            .write_all(policy_json.as_bytes())
            .and_then(|_| writer.flush())
            .context("写入策略管道失败")?;
    }
    let guard = PolicyFdGuard(unsafe { std::os::fd::OwnedFd::from_raw_fd(read_fd) });
    let raw_read = guard.0.as_raw_fd();
    // 父进程侧读端设 CLOEXEC：本进程后续 spawn 的无关子进程不会继承它。
    // SAFETY: fcntl 对有效 fd 的标准操作。
    unsafe {
        let flags = libc::fcntl(raw_read, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(raw_read, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
    // pre_exec（fork 后、exec 前）：复制到 fd3 并关闭原描述符（若非 3）。
    // dup2 语义清除目标 fd 的 CLOEXEC，fd3 随 exec 传递给沙箱程序。
    // SAFETY: pre_exec 限制内仅调用异步信号安全函数。
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(move || {
            if raw_read != 3 && libc::dup2(raw_read, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if raw_read != 3 {
                libc::close(raw_read);
            }
            Ok(())
        });
    }
    #[cfg(not(unix))]
    {
        let _ = raw_read;
        bail!("fd3 策略通道仅支持 Unix（Windows 继承句柄见 RFC S6）");
    }
    Ok(guard)
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
