//! 通用 IPC server（TCP loopback + 动态端口 + endpoint 文件 + 首帧 token 鉴权）。
//!
//! 帧层复用 `tiangong_plugin_runtime::protocol` 的通用类型，不重新定义。
//! 各 sidecar 通过实现 [`SidecarService`] trait 提供请求分发。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tiangong_plugin_runtime::protocol::{
    ErrorCode as PluginErrorCode, IpcEndpoint, IpcFrame, IpcRequest, IpcResponse,
    Request as PluginRequest, RequestInvocationContext, Response as PluginResponse,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::endpoint;
use crate::identity::SidecarConfig;
use crate::singleton::SidecarService;

tokio::task_local! {
    static REQUEST_PROGRESS: ProgressHandle;
    static REQUEST_CONTEXT: Option<RequestInvocationContext>;
}

#[derive(Clone)]
struct ProgressHandle {
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    request_id: String,
}

/// 向当前请求发送进度或 Runtime 控制反馈。TCP 与 stdio 使用同一 API；
/// 不在请求上下文中调用时静默忽略，兼容普通测试和后台通知。
pub async fn emit_progress(message: impl Into<String>) {
    let Ok(handle) = REQUEST_PROGRESS.try_with(Clone::clone) else {
        return;
    };
    let frame = IpcFrame::Progress {
        request_id: handle.request_id,
        message: message.into(),
    };
    let _ = write_shared_frame(&handle.writer, &frame).await;
}

/// 当前请求的宿主权威上下文。旧宿主或非工具调用返回 None。
pub fn invocation_context() -> Option<RequestInvocationContext> {
    REQUEST_CONTEXT.try_with(Clone::clone).ok().flatten()
}

async fn write_shared_frame(
    writer: &Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    frame: &IpcFrame,
) -> Result<()> {
    let line = serde_json::to_string(frame).with_context(|| "序列化 IPC 帧失败")?;
    let mut writer = writer.lock().await;
    writer
        .write_all(line.as_bytes())
        .await
        .with_context(|| "写入 IPC 帧失败")?;
    writer
        .write_all(b"\n")
        .await
        .with_context(|| "写入 IPC 换行失败")?;
    writer.flush().await.with_context(|| "刷新 IPC 帧失败")?;
    Ok(())
}

/// 监听中的 IPC 服务端。
struct IpcServer {
    listener: TcpListener,
    endpoint: IpcEndpoint,
    endpoint_path: PathBuf,
}

/// 已建立的双向连接。
struct IpcConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
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
        let endpoint_path = endpoint::endpoint_path(service)?;
        endpoint::persist_endpoint(&endpoint_path, &endpoint)?;
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
        // 删除前校验 endpoint 文件里的 pid 仍是本进程，避免旧实例晚退出时
        // 误删新实例刚发布的 endpoint 文件。
        if let Ok(content) = std::fs::read_to_string(&self.endpoint_path)
            && let Ok(endpoint) = serde_json::from_str::<IpcEndpoint>(&content)
            && endpoint.pid != std::process::id()
        {
            // 文件已被新实例覆盖，不删。
            return;
        }
        let _ = std::fs::remove_file(&self.endpoint_path);
    }
}

/// 启动 IPC bridge，将 [`SidecarService`] 暴露为 TCP loopback 服务。
pub fn spawn_bridge(
    config: &SidecarConfig,
    service_obj: Arc<dyn SidecarService>,
) -> Result<IpcBridge> {
    let service = config.service.clone();
    let bridge_thread_name = format!("{}-ipc-bridge-{service}", config.service);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);

    let join_handle = thread::Builder::new()
        .name(bridge_thread_name)
        .spawn({
            let service = service.clone();
            move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("sidecar IPC runtime 构建失败");
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
                    if let Err(err) = run_bridge(service, server, service_obj, shutdown_rx).await {
                        tracing::warn!("sidecar IPC bridge 退出异常: {}", err);
                    }
                });
            }
        })
        .with_context(|| format!("创建 {service} IPC bridge 线程失败"))?;

    ready_rx
        .recv()
        .with_context(|| format!("等待 {service} IPC bridge 就绪失败"))??;

    Ok(IpcBridge {
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
    })
}

async fn run_bridge(
    service: String,
    server: Arc<IpcServer>,
    service_obj: Arc<dyn SidecarService>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    tracing::info!(
        service = %service,
        port = server.endpoint.port,
        "{service} IPC bridge 已启动"
    );

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
                                tracing::debug!("IPC 连接结束: {}", err);
                            }
                        });
                    }
                    Err(err) => {
                        tracing::warn!("接受 {service} IPC 客户端失败: {}", err);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    tracing::info!(service = %service, "{service} IPC bridge 已关闭");
    Ok(())
}

/// 全局通知广播：service 代码经 [`emit_notification`] 推送，活跃连接写出
/// Notification 帧（每宿主一条连接，全部送达）。
static NOTIFICATIONS: once_link::OnceLock<tokio::sync::broadcast::Sender<(String, String)>> =
    once_link::OnceLock::new();

mod once_link {
    pub use std::sync::OnceLock;
}

fn notification_sender() -> &'static tokio::sync::broadcast::Sender<(String, String)> {
    NOTIFICATIONS.get_or_init(|| tokio::sync::broadcast::channel(256).0)
}

/// 通知广播订阅入口（stdio 模式的通知写出任务使用）。
pub(crate) fn notification_broadcast() -> &'static tokio::sync::broadcast::Sender<(String, String)>
{
    notification_sender()
}

/// service 主动推送通知（如 PTY 输出流）。无活跃订阅者时静默丢弃
/// （容量 256 满时丢最旧，调用方不需处理背压——流式场景可容忍）。
pub fn emit_notification(channel: impl Into<String>, payload: impl Into<String>) {
    let _ = notification_sender().send((channel.into(), payload.into()));
}

async fn serve_connection(
    mut connection: IpcConnection,
    service_obj: Arc<dyn SidecarService>,
) -> Result<()> {
    let mut notifications = notification_sender().subscribe();
    loop {
        let request = tokio::select! {
            request = connection.read_request() => request?,
            notification = notifications.recv() => {
                match notification {
                    Ok((channel, payload)) => {
                        connection
                            .write_frame(&IpcFrame::Notification { channel, payload })
                            .await?;
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        tracing::debug!(count, "sidecar 通知积压丢弃最旧");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // 发送端与订阅同生命周期（静态），不会关闭
                        continue;
                    }
                }
            }
        };
        let operation = request
            .payload
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let plugin_response = match serde_json::from_value::<PluginRequest>(request.payload.clone())
        {
            Ok(plugin_request) => {
                let progress = ProgressHandle {
                    writer: Arc::clone(&connection.writer),
                    request_id: request.request_id.clone(),
                };
                let context = request
                    .payload
                    .get("context")
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok());
                REQUEST_PROGRESS
                    .scope(
                        progress,
                        REQUEST_CONTEXT.scope(context, service_obj.dispatch(plugin_request)),
                    )
                    .await
            }
            Err(error) => PluginResponse::error(
                &request.request_id,
                PluginErrorCode::BadRequest,
                format!("解析插件 sidecar 请求失败: {error}"),
                false,
            ),
        };
        if let Some(err_msg) = &plugin_response.error_message {
            tracing::warn!(
                request_id = %request.request_id,
                operation,
                error_code = ?plugin_response.error_code,
                error = %err_msg,
                "sidecar 操作失败"
            );
        }
        let payload =
            serde_json::to_value(plugin_response).with_context(|| "序列化 sidecar 响应失败")?;
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
            writer: Arc::new(tokio::sync::Mutex::new(write_half)),
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
        write_shared_frame(&self.writer, frame).await
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

#[cfg(test)]
mod notification_tests {
    use super::*;

    #[test]
    fn emit_notification_available_without_subscriber() {
        // 无订阅者时发送静默丢弃，不 panic（背压安全）
        emit_notification("terminal.output", "hello");
    }
}
