//! IPC 模块（TCP loopback + 动态端口）。
//!
//! 对齐 Memory sidecar：`127.0.0.1:0` 动态端口监听、本地 endpoint 文件发现、
//! 首帧 token 鉴权。帧层复用 `tiangong_plugin_runtime::protocol` 的通用类型。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tiangong_plugin_runtime::protocol::{
    ErrorCode as PluginErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, IpcAuth, IpcEndpoint,
    IpcFrame, IpcRequest, IpcResponse, PROTOCOL_VERSION, Request as PluginRequest,
    Response as PluginResponse, ServiceStatus,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::election::home_dir;
use crate::service::McpService;

/// 监听中的 IPC 服务端。
struct IpcServer {
    listener: TcpListener,
    endpoint: IpcEndpoint,
    endpoint_path: PathBuf,
}

/// 已建立的双向连接。
struct IpcConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

/// 运行中的 IPC bridge 守卫。drop 时触发后台服务退出。
pub struct IpcBridge {
    shutdown_tx: Option<watch::Sender<bool>>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl Drop for IpcBridge {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

impl IpcServer {
    /// 绑定 `127.0.0.1:0`，创建 endpoint 文件。
    async fn bind(service: &str) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .with_context(|| "绑定 IPC TCP loopback 监听失败")?;
        let addr = listener
            .local_addr()
            .with_context(|| "读取 IPC 本地监听地址失败")?;
        let endpoint = IpcEndpoint {
            service: service.to_string(),
            host: "127.0.0.1".to_string(),
            port: addr.port(),
            pid: std::process::id(),
            token: scru128::new().to_string(),
            updated_at: chrono::Local::now().naive_local().to_string(),
        };
        let endpoint_path = endpoint_path(service)?;
        persist_endpoint(&endpoint_path, &endpoint)?;
        Ok(Self {
            listener,
            endpoint,
            endpoint_path,
        })
    }

    /// 接受一个客户端，并完成 token 鉴权。
    async fn accept_authenticated(&self) -> Result<IpcConnection> {
        let (stream, _addr) = self
            .listener
            .accept()
            .await
            .with_context(|| "接受 IPC 客户端连接失败")?;
        let mut conn = IpcConnection::from_stream(stream);
        match conn.read_frame().await? {
            IpcFrame::Auth(auth) if auth.token == self.endpoint.token => Ok(conn),
            IpcFrame::Auth(_) => {
                let _ = conn
                    .write_frame(&IpcFrame::Error {
                        message: "IPC 鉴权失败".to_string(),
                    })
                    .await;
                bail!("IPC 鉴权失败")
            }
            frame => bail!("IPC 首帧必须是 Auth，实际收到: {:?}", frame),
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.endpoint_path);
    }
}

/// 启动 MCP IPC bridge，将本地 [`McpService`] 暴露为 TCP loopback 服务。
pub fn spawn_mcp_bridge(service: impl Into<String>, service_obj: McpService) -> Result<IpcBridge> {
    let service = service.into();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);

    let join_handle = thread::Builder::new()
        .name(format!("mcp-ipc-bridge-{service}"))
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("MCP IPC runtime 构建失败");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let server = match IpcServer::bind(&service).await {
                    Ok(server) => {
                        let _ = ready_tx.send(Ok(()));
                        Arc::new(server)
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(anyhow!(err.to_string())));
                        return;
                    }
                };
                if let Err(err) = run_mcp_bridge(service, server, service_obj, shutdown_rx).await {
                    tracing::warn!("MCP IPC bridge 退出异常: {}", err);
                }
            });
        })
        .with_context(|| "创建 MCP IPC bridge 线程失败")?;

    ready_rx
        .recv()
        .with_context(|| "等待 MCP IPC bridge 就绪失败")??;

    Ok(IpcBridge {
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
    })
}

async fn run_mcp_bridge(
    service: String,
    server: Arc<IpcServer>,
    service_obj: McpService,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    tracing::info!(
        service = %service,
        port = server.endpoint.port,
        "MCP IPC bridge 已启动"
    );
    let service_obj = Arc::new(service_obj);

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    break;
                }
            }
            accepted = server.accept_authenticated() => {
                match accepted {
                    Ok(connection) => {
                        let service_obj = service_obj.clone();
                        tokio::spawn(async move {
                            if let Err(err) = serve_connection(connection, service_obj).await {
                                tracing::debug!("MCP IPC 连接结束: {}", err);
                            }
                        });
                    }
                    Err(err) => {
                        tracing::warn!("接受 MCP IPC 客户端失败: {}", err);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    tracing::info!(service = %service, "MCP IPC bridge 已关闭");
    Ok(())
}

async fn serve_connection(
    mut connection: IpcConnection,
    service_obj: Arc<McpService>,
) -> Result<()> {
    loop {
        let request = connection.read_request().await?;
        let plugin_response = match serde_json::from_value::<PluginRequest>(request.payload.clone())
        {
            Ok(plugin_request) => service_obj.dispatch(plugin_request).await,
            Err(error) => PluginResponse::error(
                &request.request_id,
                PluginErrorCode::BadRequest,
                format!("解析插件 sidecar 请求失败: {error}"),
                false,
            ),
        };
        let payload =
            serde_json::to_value(plugin_response).with_context(|| "序列化 MCP 响应失败")?;
        connection
            .write_response(IpcResponse {
                request_id: request.request_id,
                payload,
            })
            .await?;
    }
}

impl IpcConnection {
    fn from_stream(stream: TcpStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: write_half,
        }
    }

    async fn read_request(&mut self) -> Result<IpcRequest> {
        match self.read_frame().await? {
            IpcFrame::Request(req) => Ok(req),
            IpcFrame::Error { message } => bail!("IPC 对端返回错误: {message}"),
            frame => bail!("期望 Request 帧，实际收到: {:?}", frame),
        }
    }

    async fn write_response(&mut self, response: IpcResponse) -> Result<()> {
        self.write_frame(&IpcFrame::Response(response)).await
    }

    async fn write_frame(&mut self, frame: &IpcFrame) -> Result<()> {
        let line = serde_json::to_string(frame).with_context(|| "序列化 IPC 帧失败")?;
        self.writer
            .write_all(line.as_bytes())
            .await
            .with_context(|| "写入 IPC 帧失败")?;
        self.writer
            .write_all(b"\n")
            .await
            .with_context(|| "写入 IPC 换行失败")?;
        self.writer
            .flush()
            .await
            .with_context(|| "刷新 IPC 帧失败")?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<IpcFrame> {
        let mut buf = String::new();
        let bytes = self
            .reader
            .read_line(&mut buf)
            .await
            .with_context(|| "读取 IPC 帧失败")?;
        if bytes == 0 {
            bail!("IPC 连接已关闭");
        }
        serde_json::from_str(buf.trim_end()).with_context(|| "解析 IPC 帧失败")
    }
}

fn endpoint_path(service: &str) -> Result<PathBuf> {
    if let Some(path) =
        std::env::var_os("TIANGONG_PLUGIN_ENDPOINT").filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    Ok(mcp_runtime_dir()?.join(format!("{service}.json")))
}

fn mcp_runtime_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("TIANGONG_PLUGIN_DATA_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join("runtime"));
    }
    Ok(home_dir()
        .ok_or_else(|| anyhow!("无法确定 HOME/USERPROFILE"))?
        .join(".tiangong")
        .join("mcp")
        .join("runtime"))
}

fn persist_endpoint(path: &PathBuf, endpoint: &IpcEndpoint) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 IPC runtime 目录失败: {}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(endpoint).with_context(|| "序列化 IPC endpoint 失败")?;
    std::fs::write(path, content)
        .with_context(|| format!("写入 IPC endpoint 文件失败: {}", path.display()))
}
