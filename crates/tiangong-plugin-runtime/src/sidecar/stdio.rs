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
use sha2::{Digest, Sha256};

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
/// command 子进程继承的 CPU 时间上限（秒）。
pub const SANDBOX_CPU_LIMIT_ENV: &str = "TIANGONG_SANDBOX_CPU_LIMIT_SECONDS";
/// command 子进程继承的内存上限（十进制字节）。
pub const SANDBOX_MEMORY_LIMIT_ENV: &str = "TIANGONG_SANDBOX_MEMORY_LIMIT_BYTES";
/// command 子进程继承的进程数量上限。
pub const SANDBOX_PROCESS_LIMIT_ENV: &str = "TIANGONG_SANDBOX_PROCESS_LIMIT";
const PROCESS_GROUP_ENV: &str = "TIANGONG_SIDECAR_OWN_PROCESS_GROUP";

/// stdio 传输连接：单子进程 + 常驻读线程按 request_id 路由响应与进度，
/// Notification 帧走全局通知转发（与 TCP 通知监听等价）。
pub struct StdioSidecarConnection {
    config: SidecarConfig,
    state: Mutex<StdioState>,
    exec_env: Mutex<std::collections::BTreeMap<String, String>>,
    stopped: AtomicBool,
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
    #[cfg(windows)]
    job: WindowsJob,
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
            stopped: AtomicBool::new(false),
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
        // stop 是终止语义。先置位可阻止与取消并发的 invoke 在进程被杀后重启。
        self.stopped.store(true, Ordering::Release);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("stdio sidecar 状态锁已损坏"))?;
        if let Some(process) = state.process.take()
            && let Ok(mut child) = process.child.lock()
        {
            terminate_process_tree(&process, &mut child);
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
        if self.stopped.load(Ordering::Acquire) {
            bail!("stdio sidecar 已停止");
        }
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
            if let Ok(mut child) = process.child.lock() {
                terminate_process_tree(process, &mut child);
            }
            tracing::warn!(
                plugin_id = %self.config.plugin_id,
                "stdio sidecar 已退出，准备重启"
            );
            state.process = None;
        }
        let process = Arc::new(self.spawn()?);
        state.process = Some(Arc::clone(&process));
        if let Err(error) = self.handshake(&process) {
            state.process = None;
            if let Ok(mut child) = process.child.lock() {
                terminate_process_tree(&process, &mut child);
            }
            return Err(error);
        }
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
            policy.extra_writable = self.config.sandbox_extra_writable.clone();
            policy.protected_paths = vec![self.config.storage_root.clone()];
            policy.protect_user_credentials(&self.config.storage_root);
            policy
                .denied_read_paths
                .extend(self.config.sandbox_denied_read_paths.clone());
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
                let sandbox_bin = tiangong_sandbox::launcher_manager::resolve_sandbox_binary(
                    &self.config.storage_root,
                )
                .ok_or_else(|| {
                    anyhow!(
                        "插件 {} 声明沙箱但随 App 发布的 tiangong-sandbox 程序不可用，拒绝启动",
                        self.config.plugin_id
                    )
                })?;
                let request = serde_json::json!({
                    "protocol_version": 1,
                    "policy_schema": 2,
                    "policy": policy,
                    "plugin_id": self.config.plugin_id,
                    "program": self.config.binary.display().to_string(),
                    "program_root": self
                        .config
                        .sandbox_program_root
                        .as_deref()
                        .or_else(|| self.config.binary.parent())
                        .map(|path| path.display().to_string())
                        .ok_or_else(|| anyhow!("sidecar 目标程序缺少权威目录"))?,
                    "program_sha256": sha256_file(&self.config.binary)?,
                    "args": [],
                });
                let mut command = Command::new(sandbox_bin);
                policy_fd_guard = Some(prepare_policy_fd(&mut command, request.to_string())?);
                command
            }
            None => Command::new(&self.config.binary),
        };
        sanitize_spawn_environment(&mut command);
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
            .env(PLUGIN_DATA_DIR_ENV, &self.config.data_dir)
            .env(PROCESS_GROUP_ENV, "1");
        if let Some(policy) = &launch_policy {
            command
                .env(
                    SANDBOX_CPU_LIMIT_ENV,
                    policy.resource_limits.max_cpu_time_seconds.to_string(),
                )
                .env(
                    SANDBOX_MEMORY_LIMIT_ENV,
                    policy.resource_limits.max_memory_bytes.to_string(),
                )
                .env(
                    SANDBOX_PROCESS_LIMIT_ENV,
                    policy.resource_limits.max_processes.to_string(),
                );
        }
        if let Some(temp_dir) = &self.config.sandbox_temp_dir {
            if !temp_dir.is_absolute() || !temp_dir.is_dir() {
                bail!("sidecar 专用临时目录无效: {}", temp_dir.display());
            }
            command
                .env("TMPDIR", temp_dir)
                .env("TMP", temp_dir)
                .env("TEMP", temp_dir);
        }
        if self.config.allow_sensitive_storage {
            command.env(STORAGE_ROOT_ENV, &self.config.storage_root);
        }
        if let Some(env) = self.exec_env.lock().ok().filter(|env| !env.is_empty())
            && let Ok(json) = serde_json::to_string(&*env)
        {
            command.env(EXEC_ENV_JSON_ENV, json);
        }
        configure_process_lifecycle(&mut command)?;
        #[cfg(windows)]
        let job = WindowsJob::new(launch_policy.as_ref().map(|policy| policy.resource_limits))
            .context("创建 sidecar Job Object 失败")?;
        let mut child = command.spawn().with_context(|| {
            // spawn 完成：父进程侧策略管道读端即可关闭。
            format!("启动 stdio sidecar 失败: {}", self.config.binary.display())
        })?;
        #[cfg(windows)]
        if let Err(error) = job.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("将 sidecar 加入 Job Object 失败");
        }
        drop(policy_fd_guard);
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
            #[cfg(windows)]
            job,
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
                    return result.map_err(|message| {
                        let message = if message == "stdio sidecar 已关闭" {
                            format!("{message}; {}", child_status(process))
                        } else {
                            message
                        };
                        SidecarInvokeError::Internal(message).into()
                    });
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // 读线程已退出（进程死亡）：按不可用处理，下次 ensure 重启。
                    remove_pending(process, &request_id);
                    return Err(SidecarInvokeError::Unavailable(format!(
                        "stdio sidecar 读通道已关闭; {}",
                        child_status(process)
                    ))
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

fn child_status(process: &StdioProcess) -> String {
    let Ok(mut child) = process.child.lock() else {
        return "无法读取子进程状态".to_string();
    };
    match child.try_wait() {
        Ok(Some(status)) => format!("子进程退出状态: {status}"),
        Ok(None) => "子进程仍在运行但输出已关闭".to_string(),
        Err(error) => format!("读取子进程状态失败: {error}"),
    }
}

/// 为沙箱程序准备策略描述符：匿名管道写端写入策略后立即关闭（读取到
/// EOF 即策略完整）；读端经 pre_exec 复制到 fd3 并关闭原描述符。
///
/// 返回的读端守卫必须存活到 `spawn` 返回——父进程随后正常关闭（无泄漏）；
/// 标准库管道两端在返回调用方前均已设置 FD_CLOEXEC，避免并发 spawn 继承
/// 尚未关闭的写端，导致 Launcher 永远等不到策略 EOF。
#[cfg(unix)]
struct PolicyFdGuard(std::io::PipeReader);

#[cfg(not(unix))]
struct PolicyFdGuard;

#[cfg(unix)]
fn prepare_policy_fd(command: &mut Command, policy_json: String) -> Result<PolicyFdGuard> {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let (read_fd, mut writer) = std::io::pipe().context("创建策略管道失败")?;
    writer
        .write_all(policy_json.as_bytes())
        .and_then(|_| writer.flush())
        .context("写入策略管道失败")?;
    drop(writer);

    let guard = PolicyFdGuard(read_fd);
    let raw_read = guard.0.as_raw_fd();
    // pre_exec（fork 后、exec 前）：复制到 fd3 并关闭原描述符（若非 3）。
    // dup2 会清除目标 fd 的 CLOEXEC；原描述符恰好已经是 3 时则必须显式
    // 清除，否则并发 spawn 中拿到 fd3 的 Launcher 会在 exec 后读到 EBADF。
    // SAFETY: pre_exec 限制内仅调用异步信号安全函数。
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(move || {
            if raw_read == 3 {
                let flags = libc::fcntl(raw_read, libc::F_GETFD);
                if flags < 0 || libc::fcntl(raw_read, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            } else {
                if libc::dup2(raw_read, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(raw_read);
            }
            Ok(())
        });
    }
    Ok(guard)
}

#[cfg(windows)]
fn prepare_policy_fd(command: &mut Command, policy_json: String) -> Result<PolicyFdGuard> {
    command.env(tiangong_sandbox::POLICY_ENV, policy_json);
    Ok(PolicyFdGuard)
}

#[cfg(not(any(unix, windows)))]
fn prepare_policy_fd(_command: &mut Command, _policy_json: String) -> Result<PolicyFdGuard> {
    bail!("当前平台没有可用的 Launcher 策略传输通道")
}

fn sha256_file(path: &std::path::Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("读取 sidecar 目标程序失败: {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn sanitize_spawn_environment(command: &mut Command) {
    const DENIED_EXACT: &[&str] = &["BASH_ENV", "ENV", "PS4"];
    const DENIED_PREFIXES: &[&str] = &["LD_", "DYLD_"];
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if DENIED_EXACT.contains(&upper.as_str())
            || DENIED_PREFIXES
                .iter()
                .any(|prefix| upper.starts_with(prefix))
        {
            command.env_remove(key);
        }
    }
}

fn configure_process_lifecycle(command: &mut Command) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // 每个 stdio sidecar 独占进程组，正常取消时可连同 Shell 后台进程清理。
        command.process_group(0);

        #[cfg(target_os = "linux")]
        {
            let expected_parent = unsafe { libc::getpid() };
            // SAFETY: pre_exec 内仅调用异步信号安全的 libc 原语。
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // 消除 fork 与 prctl 之间父进程已经退出的竞态。
                    if libc::getppid() != expected_parent {
                        return Err(std::io::Error::other("sidecar 宿主已退出"));
                    }
                    Ok(())
                });
            }
        }
    }
    let _ = command;
    Ok(())
}

fn terminate_process_tree(process: &StdioProcess, child: &mut Child) {
    #[cfg(unix)]
    let pid = child.id();
    #[cfg(unix)]
    unsafe {
        // 进程组 ID 在 spawn 前固定为直接子进程 PID；即使组长先退出，仍可清理后代。
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    process.job.terminate();
    #[cfg(not(windows))]
    let _ = process;
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
struct WindowsJob {
    handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl WindowsJob {
    fn new(
        resource_limits: Option<tiangong_sandbox::SandboxResourceLimits>,
    ) -> std::io::Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_JOB_TIME, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW 成功后返回由当前对象独占的有效句柄。
        let job = Self {
            handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) },
        };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(resource_limits) = resource_limits {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_JOB_MEMORY
                | JOB_OBJECT_LIMIT_JOB_TIME;
            limits.BasicLimitInformation.PerJobUserTimeLimit = resource_limits
                .max_cpu_time_seconds
                .saturating_mul(10_000_000)
                as i64;
            limits.BasicLimitInformation.ActiveProcessLimit = resource_limits.max_processes;
            limits.JobMemoryLimit = resource_limits.max_memory_bytes as usize;
        }
        let configured = unsafe {
            SetInformationJobObject(
                job.raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    fn assign(&self, child: &Child) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let process = child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        if unsafe { AssignProcessToJobObject(self.raw_handle(), process) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn terminate(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        unsafe {
            TerminateJobObject(self.raw_handle(), 1);
        }
    }

    fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        self.handle.as_raw_handle()
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
        let process = {
            let mut state = self.state.lock().map_err(|_| {
                anyhow!(SidecarInvokeError::Unavailable(
                    "stdio sidecar 状态锁已损坏".to_string()
                ))
            })?;
            self.ensure_running(&mut state).map_err(|error| {
                if error.downcast_ref::<SidecarInvokeError>().is_some() {
                    error
                } else {
                    SidecarInvokeError::Unavailable(error.to_string()).into()
                }
            })?
        };
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

impl Drop for StdioSidecarConnection {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn job_object_applies_process_and_memory_limits() {
        use windows_sys::Win32::System::JobObjects::{
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_JOB_TIME, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, QueryInformationJobObject,
        };

        let expected = tiangong_sandbox::SandboxResourceLimits::default();
        let job = WindowsJob::new(Some(expected)).unwrap();
        let mut actual: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let queried = unsafe {
            QueryInformationJobObject(
                job.raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw mut actual).cast(),
                std::mem::size_of_val(&actual) as u32,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(queried, 0);
        assert_ne!(
            actual.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            0
        );
        assert_ne!(
            actual.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_MEMORY,
            0
        );
        assert_ne!(
            actual.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_JOB_TIME,
            0
        );
        assert_eq!(
            actual.BasicLimitInformation.PerJobUserTimeLimit,
            expected.max_cpu_time_seconds as i64 * 10_000_000
        );
        assert_eq!(
            actual.BasicLimitInformation.ActiveProcessLimit,
            expected.max_processes
        );
        assert_eq!(actual.JobMemoryLimit, expected.max_memory_bytes as usize);
    }
}
