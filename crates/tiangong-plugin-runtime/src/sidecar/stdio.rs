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

use crate::protocol::{
    HANDSHAKE_OPERATION, HandshakeResponse, IpcAuth, IpcFrame, IpcRequest, PROTOCOL_VERSION,
    Request, Response,
};
use crate::sidecar::{
    EXEC_ENV_JSON_ENV, PLUGIN_DATA_DIR_ENV, PLUGIN_ENDPOINT_ENV, PLUGIN_ID_ENV, PLUGIN_VERSION_ENV,
    STORAGE_ROOT_ENV, SidecarConfig, SidecarConnection, SidecarInvokeError,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

/// 子进程环境：传输模式标记。
pub const TRANSPORT_ENV: &str = "TIANGONG_PLUGIN_TRANSPORT";
/// 子进程环境：stdio 模式的认证 token。
pub const STDIO_TOKEN_ENV: &str = "TIANGONG_PLUGIN_STDIO_TOKEN";
/// 子进程环境：创建并持有 stdio 连接的宿主进程 PID。
pub const HOST_PID_ENV: &str = "TIANGONG_PLUGIN_HOST_PID";
pub const TRANSPORT_STDIO: &str = "stdio";
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
    lifecycle: WindowsJob,
}

#[derive(Clone)]
struct PendingWaiter {
    response: SyncSender<Result<Value, String>>,
    progress: SyncSender<String>,
    invocation: Option<crate::sidecar::SidecarInvocationContext>,
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

    /// 确保进程存活并完成握手（安装验证 / 预热用）。
    ///
    /// 按需生命周期不保留进程：临时启动、握手校验后立即清理——预热/安装
    /// 验证仍确认可达性，但不留下任何存活进程。
    pub fn ensure_running_checked(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("stdio sidecar 状态锁已损坏"))?;
        match self.config.lifecycle {
            crate::manifest::SidecarLifecycle::OnDemand => {
                if self.stopped.load(Ordering::Acquire) {
                    bail!("stdio sidecar 已停止");
                }
                // 临时校验进程完全不经过共享 state：按需调用可能正在进行
                //（state.process 是其活跃进程），经 state 启停会误杀它。
                let process = Arc::new(self.spawn()?);
                let result = self.handshake(&process);
                if let Ok(mut child) = process.child.lock() {
                    terminate_process_tree(&process, &mut child);
                }
                result.map(|_| ())
            }
            crate::manifest::SidecarLifecycle::Resident => {
                self.ensure_running(&mut state).map(|_| ())
            }
        }
    }

    /// 确保子进程存活且完成过握手。进程退出时自动重启（换代）。
    fn ensure_running(&self, state: &mut StdioState) -> Result<Arc<StdioProcess>> {
        if self.stopped.load(Ordering::Acquire) {
            bail!("stdio sidecar 已停止");
        }
        if let Some(process) = state.process.as_ref() {
            // 共享状态中仍有进程代次即直接复用；退出由读线程关闭 pending，
            // 下一次写失败后再换代。这里不查询 Child，避免并发请求等待进程锁。
            return Ok(Arc::clone(process));
        }
        self.start_fresh(state)
    }

    /// 启动全新进程并完成握手（写入 state.process）。
    /// 覆盖前先终止旧进程——预热残留或异常路径留下的进程不允许失去管理引用。
    fn start_fresh(&self, state: &mut StdioState) -> Result<Arc<StdioProcess>> {
        if let Some(process) = state.process.take()
            && let Ok(mut child) = process.child.lock()
        {
            terminate_process_tree(&process, &mut child);
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

    /// 按需调用：每次请求独立进程（spawn → 握手 → 请求 → 清理），
    /// 不复用也不保留进程——工具型调用的最小存活窗口。
    ///
    /// 锁只在起止瞬间持有：round_trip 期间不持锁，工具级超时/会话取消
    /// （cancel_current）才能及时终止进行中的进程而不与调用方互等。
    fn invoke_on_demand(
        &self,
        operation: &str,
        payload: Value,
        invocation: Option<crate::sidecar::SidecarInvocationContext>,
        on_progress: &mut dyn FnMut(String),
    ) -> Result<Value> {
        let process = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("stdio sidecar 状态锁已损坏"))?;
            if self.stopped.load(Ordering::Acquire) {
                bail!("stdio sidecar 已停止");
            }
            self.start_fresh(&mut state)?
        };
        let result = self.round_trip(&process, operation, payload, invocation, on_progress);
        // 先清理进程，再获取 state 锁；读线程在发送关闭错误时可能短暂持有
        // pending 锁，反向持锁会让请求收尾与取消互相等待。
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("stdio sidecar 状态锁已损坏"))?;
        if state
            .process
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &process))
        {
            state.process = None;
        }
        result
    }

    /// 按宿主会话取消其仍在执行的请求。常驻 sidecar 发送请求级 Cancel，
    /// 不杀进程；按需 sidecar 每次调用独占进程，直接清理进程树。
    pub fn cancel_session(&self, session_id: &str) -> Result<()> {
        let process = self
            .state
            .lock()
            .map_err(|_| anyhow!("stdio sidecar 状态锁已损坏"))?
            .process
            .clone();
        let Some(process) = process else {
            return Ok(());
        };
        if self.config.lifecycle == crate::manifest::SidecarLifecycle::OnDemand {
            self.cancel_current();
            return Ok(());
        }
        let request_ids = process
            .pending
            .lock()
            .map_err(|_| anyhow!("stdio sidecar pending 锁已损坏"))?
            .iter()
            .filter(|(_, waiter)| {
                waiter
                    .invocation
                    .as_ref()
                    .is_some_and(|context| context.session_id == session_id)
            })
            .map(|(request_id, _)| request_id.clone())
            .collect::<Vec<_>>();
        for request_id in request_ids {
            let waiter = process
                .pending
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&request_id));
            self.cancel_request(&process, &request_id)?;
            if let Some(waiter) = waiter {
                let _ = waiter.response.send(Err("请求已取消".to_string()));
            }
        }
        Ok(())
    }

    /// 终止当前进行中的调用进程（工具级超时 / 会话取消用）：
    /// 与 stop 不同，不改变停止标志——后续调用会重新起进程继续服务。
    pub fn cancel_current(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(process) = state.process.take()
            && let Ok(mut child) = process.child.lock()
        {
            terminate_process_tree(&process, &mut child);
        }
    }

    fn spawn(&self) -> Result<StdioProcess> {
        match self.spawn_once() {
            Ok(process) => Ok(process),
            Err(SpawnAttemptError::Preparation(error)) => Err(error),
            Err(SpawnAttemptError::ProcessCreation { program, source }) => {
                // 仅进程创建失败（文件被删、无执行权限、格式无效等）才
                // 走恢复接口：缓存匹配失效 → 排除失败路径重探；恢复出新
                // 路径后重试一次。spawn_once 每次都经缓存入口取最新路径，
                // 后续重启自然使用新路径。
                if let Some(launch) = self.config.interpreter.as_ref()
                    && crate::interpreter_env::recover_interpreter_after_spawn_failure(
                        launch.kind,
                        &program,
                    )
                    .is_some()
                {
                    // 第二次失败返回真实错误，不再第三次恢复；若仍是
                    // 解释器创建失败，清掉刚恢复的新缓存（已知坏路径）。
                    return match self.spawn_once() {
                        Ok(process) => Ok(process),
                        Err(SpawnAttemptError::Preparation(error)) => Err(error),
                        Err(SpawnAttemptError::ProcessCreation {
                            program: second_program,
                            source: second_source,
                        }) => {
                            if let Some(launch) = self.config.interpreter.as_ref() {
                                crate::interpreter_env::invalidate_if_matches(
                                    launch.kind,
                                    &second_program,
                                );
                            }
                            Err(second_source)
                        }
                    };
                }
                Err(source)
            }
        }
    }

    /// 解释器形态每次启动都经缓存入口解析程序路径（命中只做一次
    /// `is_file` 校验，不触发目录扫描），不在配置中保存可能过期的
    /// 解释器路径副本。
    fn spawn_once(&self) -> std::result::Result<StdioProcess, SpawnAttemptError> {
        if !self.config.binary.is_file() {
            return Err(SpawnAttemptError::Preparation(anyhow!(
                "sidecar 二进制不存在: {}",
                self.config.binary.display()
            )));
        }
        if let Some(parent) = self.config.log.parent() {
            preparation(
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建 sidecar 日志目录失败: {}", parent.display())),
            )?;
        }
        preparation(
            std::fs::create_dir_all(&self.config.data_dir).with_context(|| {
                format!(
                    "创建 sidecar 数据目录失败: {}",
                    self.config.data_dir.display()
                )
            }),
        )?;
        let stderr = preparation(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.config.log)
                .with_context(|| format!("打开 sidecar 日志失败: {}", self.config.log.display())),
        )?;

        let token = scru128::new().to_string();
        // OS 沙箱路径（经 tiangong-sandbox Launcher 启动）由沙箱覆盖分支
        // 在本传输层之上叠加；此处仅负责直接 spawn 与 stdio 管道接续。
        // 解释器形态：以宿主缓存解析的解释器程序运行 entry（本地信任时
        // 先复核内容清单）。
        let (program, mut command) = match self.config.interpreter.as_ref() {
            Some(launch) => {
                if let Some(manifest_path) = &self.config.integrity_manifest {
                    let root = preparation(
                        manifest_path
                            .parent()
                            .ok_or_else(|| anyhow!("内容清单缺少父目录")),
                    )?;
                    preparation(SidecarConfig::verify_integrity_manifest(
                        manifest_path,
                        root,
                    ))?;
                }
                let program =
                    preparation(crate::interpreter_env::resolve_interpreter(launch.kind))?;
                if !program.is_file() {
                    return Err(SpawnAttemptError::Preparation(anyhow!(
                        "解释器程序不存在: {}（可用 TIANGONG_NODE_PATH/TIANGONG_PYTHON_PATH 指定）",
                        program.display()
                    )));
                }
                if !launch.entry.is_file() {
                    return Err(SpawnAttemptError::Preparation(anyhow!(
                        "sidecar 入口脚本不存在: {}",
                        launch.entry.display()
                    )));
                }
                let mut command = Command::new(&program);
                command.arg(&launch.entry);
                command.args(&launch.args);
                (program, command)
            }
            None => {
                if !self.config.binary.is_file() {
                    return Err(SpawnAttemptError::Preparation(anyhow!(
                        "sidecar 二进制不存在: {}",
                        self.config.binary.display()
                    )));
                }
                (
                    self.config.binary.clone(),
                    Command::new(&self.config.binary),
                )
            }
        };
        sanitize_spawn_environment(&mut command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .env(TRANSPORT_ENV, TRANSPORT_STDIO)
            .env(STDIO_TOKEN_ENV, &token)
            .env(HOST_PID_ENV, std::process::id().to_string())
            .env(PLUGIN_ID_ENV, &self.config.plugin_id)
            .env(PLUGIN_VERSION_ENV, &self.config.plugin_version)
            // stdio 模式无 endpoint 文件；保留路径占位以兼容读取方。
            .env(PLUGIN_ENDPOINT_ENV, &self.config.endpoint)
            .env(PLUGIN_DATA_DIR_ENV, &self.config.data_dir)
            .env(PROCESS_GROUP_ENV, "1");
        // 运行期解释器环境的权威来源是缓存：不修改宿主全局环境，仅对
        // 新建子进程注入覆盖（TIANGONG_*_PATH + 前置解释器目录的 PATH），
        // 恢复后的新路径由此传导给 sidecar 及其派生的命令通道进程。
        for (key, value) in crate::interpreter_env::child_env_overrides() {
            command.env(key, value);
        }
        if let Some(temp_dir) = &self.config.sandbox_temp_dir {
            if !temp_dir.is_absolute() || !temp_dir.is_dir() {
                return Err(SpawnAttemptError::Preparation(anyhow!(
                    "sidecar 专用临时目录无效: {}",
                    temp_dir.display()
                )));
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
        preparation(configure_process_lifecycle(&mut command))?;
        #[cfg(windows)]
        let lifecycle = preparation(WindowsJob::new().context("创建 sidecar Job Object 失败"))?;
        let mut child = command.spawn().map_err(|error| {
            let context = format!("启动 stdio sidecar 失败: {}", program.display());
            SpawnAttemptError::ProcessCreation {
                program,
                source: anyhow::Error::new(error).context(context),
            }
        })?;
        #[cfg(windows)]
        if let Err(error) = lifecycle.assign(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SpawnAttemptError::Preparation(
                Err::<(), _>(error)
                    .context("将 sidecar 加入 Job Object 失败")
                    .unwrap_err(),
            ));
        }
        let stdin = preparation(child.stdin.take().context("stdio sidecar 未提供 stdin"))?;
        let stdout = preparation(child.stdout.take().context("stdio sidecar 未提供 stdout"))?;
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
            lifecycle,
        })
    }

    /// 握手校验身份（plugin_id / 协议版本），对齐 TCP health_check。
    fn handshake(&self, process: &Arc<StdioProcess>) -> Result<()> {
        let payload = self.round_trip(
            process,
            HANDSHAKE_OPERATION,
            serde_json::Value::Null,
            None,
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
        // 对齐 TCP health_check：插件版本与业务协议版本一并校验，旧制品在
        // 握手阶段即明确拒绝，不留到业务调用以不确定方式失败。
        if handshake.plugin_version != self.config.plugin_version {
            return Err(SidecarInvokeError::ProtocolMismatch(format!(
                "stdio sidecar 插件版本不匹配: expected={}, actual={}",
                self.config.plugin_version, handshake.plugin_version
            ))
            .into());
        }
        if handshake.business_protocol != self.config.business_protocol {
            return Err(SidecarInvokeError::ProtocolMismatch(format!(
                "stdio sidecar 业务协议版本不匹配: expected={}, actual={}",
                self.config.business_protocol, handshake.business_protocol
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
        invocation: Option<crate::sidecar::SidecarInvocationContext>,
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
                    invocation,
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

    fn write_frame(&self, process: &StdioProcess, frame: &IpcFrame) -> Result<()> {
        let mut stdin = process
            .stdin
            .lock()
            .map_err(|_| anyhow!("stdio sidecar 写端锁已损坏"))?;
        write_line(&mut stdin, frame)
    }

    fn cancel_request(&self, process: &StdioProcess, request_id: &str) -> Result<()> {
        self.write_frame(
            process,
            &IpcFrame::Cancel {
                request_id: request_id.to_string(),
            },
        )?;
        Ok(())
    }
}

fn child_status(process: &StdioProcess) -> String {
    match process.child.try_lock() {
        Ok(mut child) => match child.try_wait() {
            Ok(Some(status)) => format!("子进程退出状态: {status}"),
            Ok(None) => "子进程仍在运行但输出已关闭".to_string(),
            Err(error) => format!("读取子进程状态失败: {error}"),
        },
        Err(std::sync::TryLockError::WouldBlock) => "子进程仍在运行（状态查询忙）".to_string(),
        Err(std::sync::TryLockError::Poisoned(_)) => "无法读取子进程状态".to_string(),
    }
}

/// sidecar 启动尝试的错误分类：只有进程创建失败才允许失效解释器缓存
/// 并重试，前置准备失败与解释器发现无关。
enum SpawnAttemptError {
    /// `Command::spawn()` 失败（文件被删、无执行权限、程序格式无效等），
    /// 携带实际尝试的程序路径。
    ProcessCreation {
        program: std::path::PathBuf,
        source: anyhow::Error,
    },
    /// 前置准备失败（目录/日志/清单校验/生命周期配置等）。
    Preparation(anyhow::Error),
}

fn preparation<T>(result: anyhow::Result<T>) -> std::result::Result<T, SpawnAttemptError> {
    result.map_err(SpawnAttemptError::Preparation)
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
    // Windows 侧 Job Object 整组终止（KILL_ON_JOB_CLOSE + 显式 Terminate），
    // 不需要子进程句柄；随后的 child.kill/wait 对已死进程为 no-op。
    #[cfg(windows)]
    process.lifecycle.terminate();
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
    fn new() -> std::io::Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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
    if let Ok(mut pending) = process.pending.try_lock() {
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
                        let waiter = pending
                            .lock()
                            .ok()
                            .and_then(|mut map| map.remove(&response.request_id));
                        if let Some(waiter) = waiter {
                            let parsed = parse_response_payload(response.payload);
                            let _ = waiter.response.send(parsed);
                        }
                    }
                    IpcFrame::Progress {
                        request_id,
                        message,
                    } => {
                        let waiter = pending
                            .lock()
                            .ok()
                            .and_then(|map| map.get(&request_id).cloned());
                        if let Some(waiter) = waiter {
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
                    IpcFrame::Auth(_) | IpcFrame::Request(_) | IpcFrame::Cancel { .. } => {
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
        let response = match self.config.lifecycle {
            crate::manifest::SidecarLifecycle::OnDemand => self
                .invoke_on_demand(operation, payload, None, on_progress)
                .map_err(|error| {
                    if error.downcast_ref::<SidecarInvokeError>().is_some() {
                        error
                    } else {
                        SidecarInvokeError::Unavailable(error.to_string()).into()
                    }
                })?,
            crate::manifest::SidecarLifecycle::Resident => {
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
                self.round_trip(&process, operation, payload, None, on_progress)
                    .map_err(|error| {
                        if error.downcast_ref::<SidecarInvokeError>().is_some() {
                            error
                        } else {
                            SidecarInvokeError::Internal(error.to_string()).into()
                        }
                    })?
            }
        };
        serde_json::to_string(&response).with_context(|| "序列化 sidecar 响应失败")
    }

    fn invoke_with_context(
        &self,
        operation: &str,
        payload: &str,
        context: &crate::sidecar::SidecarInvocationContext,
    ) -> Result<String> {
        self.invoke_with_context_and_progress(operation, payload, context, &mut |_| {})
    }

    fn invoke_with_context_and_progress(
        &self,
        operation: &str,
        payload: &str,
        context: &crate::sidecar::SidecarInvocationContext,
        on_progress: &mut dyn FnMut(String),
    ) -> Result<String> {
        let payload = serde_json::from_str(payload).with_context(|| "sidecar 请求不是有效 JSON")?;
        let response = match self.config.lifecycle {
            crate::manifest::SidecarLifecycle::OnDemand => {
                self.invoke_on_demand(operation, payload, Some(context.clone()), on_progress)?
            }
            crate::manifest::SidecarLifecycle::Resident => {
                let process = {
                    let mut state = self
                        .state
                        .lock()
                        .map_err(|_| anyhow!("stdio sidecar 状态锁已损坏"))?;
                    self.ensure_running(&mut state)?
                };
                self.round_trip(
                    &process,
                    operation,
                    payload,
                    Some(context.clone()),
                    on_progress,
                )?
            }
        };
        serde_json::to_string(&response).with_context(|| "序列化 sidecar 响应失败")
    }

    fn update_exec_env(&self, env: std::collections::BTreeMap<String, String>) {
        StdioSidecarConnection::update_exec_env(self, env);
    }

    fn stop(&self) -> Result<()> {
        StdioSidecarConnection::stop(self)
    }

    fn cancel_session(&self, session_id: &str) -> Result<()> {
        StdioSidecarConnection::cancel_session(self, session_id)
    }

    fn cancel_current(&self) {
        StdioSidecarConnection::cancel_current(self)
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
    fn job_object_kills_children_on_close() {
        use windows_sys::Win32::System::JobObjects::{
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, QueryInformationJobObject,
        };

        let job = WindowsJob::new().unwrap();
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
            actual.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            0
        );
    }
}
