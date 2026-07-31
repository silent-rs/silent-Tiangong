//! 通用插件 sidecar 进程与连接管理。
//!
//! 本模块只处理进程、endpoint、鉴权和 JSON Lines 传输，不理解插件业务协议。

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::protocol::{
    HANDSHAKE_OPERATION, HandshakeResponse, IpcAuth, IpcEndpoint, IpcFrame, IpcRequest,
    PROTOCOL_VERSION, Request, Response,
};

const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 通用 sidecar 连接：运行时负责协议封装，调用方只提供操作名和 JSON 负载。
pub trait SidecarConnection: Send + Sync {
    fn invoke(&self, operation: &str, payload: &str) -> Result<String>;
}

/// 一个插件 sidecar 的本地运行配置。
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    pub plugin_id: String,
    pub binary: PathBuf,
    pub endpoint: PathBuf,
    pub log: PathBuf,
    pub start_timeout: Duration,
    pub request_timeout: Duration,
}

impl SidecarConfig {
    pub fn new(
        plugin_id: impl Into<String>,
        binary: impl Into<PathBuf>,
        endpoint: impl Into<PathBuf>,
        log: impl Into<PathBuf>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            binary: binary.into(),
            endpoint: endpoint.into(),
            log: log.into(),
            start_timeout: DEFAULT_START_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

/// 通过 endpoint 文件连接本地 sidecar，并在不可用时负责启动。
pub struct ProcessSidecarConnection {
    config: SidecarConfig,
    start_lock: Mutex<()>,
}

impl ProcessSidecarConnection {
    pub fn new(config: SidecarConfig) -> Self {
        Self {
            config,
            start_lock: Mutex::new(()),
        }
    }

    pub fn ensure_running(&self) -> Result<()> {
        if self.health_check().is_ok() {
            return Ok(());
        }

        let _guard = self
            .start_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("sidecar 启动锁已损坏"))?;
        if self.health_check().is_ok() {
            return Ok(());
        }

        let _ = std::fs::remove_file(&self.config.endpoint);
        self.spawn()?;

        let deadline = Instant::now() + self.config.start_timeout;
        loop {
            if self.health_check().is_ok() {
                tracing::info!(plugin_id = %self.config.plugin_id, "插件 sidecar 已就绪");
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "等待插件 {} sidecar 就绪超时，日志：{}",
                    self.config.plugin_id,
                    self.config.log.display()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn health_check(&self) -> Result<()> {
        let payload = self.invoke_protocol_once(
            HANDSHAKE_OPERATION,
            serde_json::json!({"plugin_id": self.config.plugin_id}),
        )?;
        let handshake: HandshakeResponse =
            serde_json::from_value(payload).with_context(|| "解析 sidecar 握手响应失败")?;
        if handshake.plugin_id != self.config.plugin_id {
            bail!(
                "sidecar 插件标识不匹配: expected={}, actual={}",
                self.config.plugin_id,
                handshake.plugin_id
            );
        }
        if handshake.protocol_version != PROTOCOL_VERSION {
            bail!(
                "sidecar 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                handshake.protocol_version
            );
        }
        Ok(())
    }

    fn spawn(&self) -> Result<()> {
        if !self.config.binary.is_file() {
            bail!("sidecar 二进制不存在: {}", self.config.binary.display());
        }
        if let Some(parent) = self.config.log.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建 sidecar 日志目录失败: {}", parent.display()))?;
        }
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.log)
            .with_context(|| format!("打开 sidecar 日志失败: {}", self.config.log.display()))?;
        let stderr = stdout
            .try_clone()
            .with_context(|| "复制 sidecar 日志句柄失败")?;

        let mut command = Command::new(&self.config.binary);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        configure_detached(&mut command);

        let mut child = command
            .spawn()
            .with_context(|| format!("启动 sidecar 失败: {}", self.config.binary.display()))?;
        let pid = child.id();
        tracing::info!(
            plugin_id = %self.config.plugin_id,
            pid,
            binary = %self.config.binary.display(),
            "插件 sidecar 已启动"
        );
        std::thread::Builder::new()
            .name(format!("plugin-sidecar-reaper-{}", self.config.plugin_id))
            .spawn(move || {
                if let Ok(status) = child.wait() {
                    tracing::warn!(pid, %status, "插件 sidecar 已退出");
                }
            })
            .with_context(|| "创建 sidecar 回收线程失败")?;
        Ok(())
    }

    fn invoke_protocol_once(
        &self,
        operation: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let endpoint = load_endpoint(&self.config.endpoint)?;
        let mut stream = connect(&endpoint, self.config.request_timeout)?;
        write_frame(
            &mut stream,
            &IpcFrame::Auth(IpcAuth {
                token: endpoint.token,
            }),
        )?;

        let request = Request::new(operation, payload);
        let request_id = request.request_id.clone();
        write_frame(
            &mut stream,
            &IpcFrame::Request(IpcRequest {
                request_id: request_id.clone(),
                payload: serde_json::to_value(request)
                    .with_context(|| "序列化 sidecar 协议请求失败")?,
            }),
        )?;

        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .with_context(|| "读取 sidecar 响应失败")?;
        if line.is_empty() {
            bail!("sidecar 在返回响应前关闭连接");
        }
        match serde_json::from_str::<IpcFrame>(line.trim_end())
            .with_context(|| "解析 sidecar 响应帧失败")?
        {
            IpcFrame::Response(response) if response.request_id == request_id => {
                let response: Response = serde_json::from_value(response.payload)
                    .with_context(|| "解析 sidecar 协议响应失败")?;
                if response.protocol_version != PROTOCOL_VERSION {
                    bail!(
                        "sidecar 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                        response.protocol_version
                    );
                }
                if response.request_id != request_id {
                    bail!(
                        "sidecar 协议响应编号不匹配: expected={request_id}, actual={}",
                        response.request_id
                    );
                }
                if !response.success {
                    bail!(
                        "{}",
                        response
                            .error_message
                            .unwrap_or_else(|| "sidecar 请求失败".to_string())
                    );
                }
                Ok(response.payload.unwrap_or(serde_json::Value::Null))
            }
            IpcFrame::Response(response) => bail!(
                "sidecar 响应编号不匹配: expected={request_id}, actual={}",
                response.request_id
            ),
            IpcFrame::Error { message } => bail!("sidecar 返回错误: {message}"),
            _ => bail!("sidecar 返回了无效响应帧"),
        }
    }
}

impl SidecarConnection for ProcessSidecarConnection {
    fn invoke(&self, operation: &str, payload: &str) -> Result<String> {
        self.ensure_running()?;
        let payload = serde_json::from_str(payload).with_context(|| "sidecar 请求不是有效 JSON")?;
        let response = self.invoke_protocol_once(operation, payload)?;
        serde_json::to_string(&response).with_context(|| "序列化 sidecar 响应失败")
    }
}

fn load_endpoint(path: &Path) -> Result<IpcEndpoint> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("读取 sidecar endpoint 失败: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("解析 sidecar endpoint 失败: {}", path.display()))
}

fn connect(endpoint: &IpcEndpoint, timeout: Duration) -> Result<TcpStream> {
    let address = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .with_context(|| "解析 sidecar 地址失败")?
        .next()
        .ok_or_else(|| anyhow::anyhow!("sidecar 地址为空"))?;
    connect_address(address, timeout)
}

fn connect_address(address: SocketAddr, timeout: Duration) -> Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&address, timeout)
        .with_context(|| format!("连接 sidecar 失败: {address}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .with_context(|| "设置 sidecar 读取超时失败")?;
    stream
        .set_write_timeout(Some(timeout))
        .with_context(|| "设置 sidecar 写入超时失败")?;
    Ok(stream)
}

fn write_frame(stream: &mut TcpStream, frame: &IpcFrame) -> Result<()> {
    serde_json::to_writer(&mut *stream, frame).with_context(|| "序列化 sidecar 请求帧失败")?;
    stream
        .write_all(b"\n")
        .with_context(|| "写入 sidecar 请求失败")?;
    stream.flush().with_context(|| "刷新 sidecar 请求失败")
}

#[cfg(unix)]
fn configure_detached(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: pre_exec 中只调用 async-signal-safe 的 setsid。
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn configure_detached(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
}

#[cfg(not(any(unix, windows)))]
fn configure_detached(_command: &mut Command) {}
