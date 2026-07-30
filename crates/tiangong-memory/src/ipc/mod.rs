//! IPC 模块（TCP loopback + 动态端口）
//!
//! 为保证 Windows / macOS / Linux 跨平台一致性，Memory IPC 统一使用：
//! - `127.0.0.1:0` 动态端口监听
//! - 本地 endpoint 文件发现
//! - 首帧 token 鉴权

pub mod protocol;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use protocol::{
    IpcAuth, IpcEndpoint, IpcFrame, IpcRequest, IpcResponse, MemoryIpcRequestPayload,
    MemoryIpcResponsePayload,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::handle::MemoryHandle;

/// 监听中的 IPC 服务端。
pub struct IpcServer {
    listener: TcpListener,
    endpoint: IpcEndpoint,
    endpoint_path: PathBuf,
}

/// IPC 客户端，建立连接后即完成鉴权。
pub struct IpcClient {
    connection: IpcConnection,
}

/// 已建立的双向连接。
pub struct IpcConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

/// 运行中的 IPC bridge 守卫。drop 时会触发后台服务退出。
pub struct IpcBridge {
    shutdown_tx: Option<watch::Sender<bool>>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl IpcServer {
    /// 绑定 `127.0.0.1:0`，创建 endpoint 文件。
    pub async fn bind(service: &str) -> Result<Self> {
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

    pub fn endpoint(&self) -> &IpcEndpoint {
        &self.endpoint
    }

    pub fn endpoint_path(&self) -> &PathBuf {
        &self.endpoint_path
    }

    /// 接受一个客户端，并完成 token 鉴权。
    pub async fn accept_authenticated(&self) -> Result<IpcConnection> {
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

impl IpcClient {
    /// 读取 endpoint 文件并连接。
    pub async fn connect(service: &str) -> Result<Self> {
        let endpoint = load_endpoint(service)?;
        Self::connect_endpoint(&endpoint).await
    }

    /// 使用给定 endpoint 建立连接并发送鉴权首帧。
    pub async fn connect_endpoint(endpoint: &IpcEndpoint) -> Result<Self> {
        let stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
            .await
            .with_context(|| format!("连接 IPC 服务失败: {}:{}", endpoint.host, endpoint.port))?;
        let mut connection = IpcConnection::from_stream(stream);
        connection
            .write_frame(&IpcFrame::Auth(IpcAuth {
                token: endpoint.token.clone(),
            }))
            .await?;
        Ok(Self { connection })
    }

    pub async fn send_request(&mut self, request: IpcRequest) -> Result<()> {
        self.connection
            .write_frame(&IpcFrame::Request(request))
            .await
    }

    pub async fn read_response(&mut self) -> Result<IpcResponse> {
        match self.connection.read_frame().await? {
            IpcFrame::Response(resp) => Ok(resp),
            IpcFrame::Error { message } => bail!("IPC 服务端返回错误: {message}"),
            frame => bail!("期望 Response 帧，实际收到: {:?}", frame),
        }
    }
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

/// 启动 Memory IPC bridge，将本地 `MemoryHandle` 暴露为 TCP loopback 服务。
pub fn spawn_memory_bridge(service: impl Into<String>, handle: MemoryHandle) -> Result<IpcBridge> {
    let service = service.into();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);

    let join_handle = thread::Builder::new()
        .name(format!("memory-ipc-bridge-{service}"))
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Memory IPC runtime 构建失败");
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
                if let Err(err) = run_memory_bridge(service, server, handle, shutdown_rx).await {
                    tracing::warn!("Memory IPC bridge 退出异常: {}", err);
                }
            });
        })
        .with_context(|| "创建 Memory IPC bridge 线程失败")?;

    ready_rx
        .recv()
        .with_context(|| "等待 Memory IPC bridge 就绪失败")??;

    Ok(IpcBridge {
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
    })
}

async fn run_memory_bridge(
    service: String,
    server: Arc<IpcServer>,
    handle: MemoryHandle,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    tracing::info!(
        service = %service,
        endpoint = %server.endpoint_path().display(),
        port = server.endpoint().port,
        "Memory IPC bridge 已启动"
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
                        let handle = handle.clone();
                        tokio::spawn(async move {
                            if let Err(err) = serve_connection(connection, handle).await {
                                tracing::debug!("Memory IPC 连接结束: {}", err);
                            }
                        });
                    }
                    Err(err) => {
                        tracing::warn!("接受 Memory IPC 客户端失败: {}", err);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    tracing::info!(service = %service, "Memory IPC bridge 已关闭");
    Ok(())
}

async fn serve_connection(mut connection: IpcConnection, handle: MemoryHandle) -> Result<()> {
    loop {
        let request = connection.read_request().await?;
        let payload: MemoryIpcRequestPayload = serde_json::from_value(request.payload)
            .with_context(|| "解析 Memory IPC 请求载荷失败")?;
        let response_payload = handle_memory_request(handle.clone(), payload).await?;
        connection
            .write_response(IpcResponse {
                request_id: request.request_id,
                payload: serde_json::to_value(response_payload)
                    .with_context(|| "序列化 Memory IPC 响应载荷失败")?,
            })
            .await?;
    }
}

pub async fn handle_memory_request(
    handle: MemoryHandle,
    payload: MemoryIpcRequestPayload,
) -> Result<MemoryIpcResponsePayload> {
    match payload {
        MemoryIpcRequestPayload::LoadInjection {
            session_id,
            workspace_id,
        } => Ok(MemoryIpcResponsePayload::Injection {
            items: handle
                .load_injection(&session_id, workspace_id.as_deref())
                .await,
        }),
        MemoryIpcRequestPayload::Recall { anchors, limit } => {
            Ok(MemoryIpcResponsePayload::Recall {
                hits: handle.recall(anchors, limit).await,
            })
        }
        MemoryIpcRequestPayload::RecallContext { request } => {
            Ok(MemoryIpcResponsePayload::RecallContext {
                response: handle.recall_context(request).await,
            })
        }
        MemoryIpcRequestPayload::RoughRecall { context } => Ok(MemoryIpcResponsePayload::Recall {
            hits: handle.rough_recall(context).await,
        }),
        MemoryIpcRequestPayload::EvaluateRecallSufficiency {
            context,
            rough_hits,
        } => Ok(MemoryIpcResponsePayload::RecallSufficiency {
            result: handle
                .evaluate_recall_sufficiency(context, rough_hits)
                .await,
        }),
        MemoryIpcRequestPayload::LoadDepth2 { node_ids } => Ok(MemoryIpcResponsePayload::Depth2 {
            items: handle.load_depth2(node_ids).await,
        }),
        MemoryIpcRequestPayload::ListNodes { query } => Ok(MemoryIpcResponsePayload::Nodes {
            items: handle.list_nodes(query).await,
        }),
        MemoryIpcRequestPayload::CountNodes { query } => Ok(MemoryIpcResponsePayload::NodeCount {
            count: handle.count_nodes(query).await,
        }),
        MemoryIpcRequestPayload::ListRelations { node_id } => {
            Ok(MemoryIpcResponsePayload::Relations {
                items: handle.list_relations(node_id).await,
            })
        }
        MemoryIpcRequestPayload::ListRelationsBatch { node_ids } => {
            Ok(MemoryIpcResponsePayload::Relations {
                items: handle.list_relations_batch(node_ids).await,
            })
        }
        MemoryIpcRequestPayload::WriteEpisode {
            episode,
            workspace_id,
        } => {
            handle.write_episode(episode, workspace_id);
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::UpsertManualMemory { draft } => {
            Ok(MemoryIpcResponsePayload::Node {
                item: handle.upsert_manual_memory(draft).await?,
            })
        }
        MemoryIpcRequestPayload::SetNodeStatus { node_id, status } => {
            handle.set_node_status(node_id, status).await?;
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::UpsertRelation { draft } => {
            Ok(MemoryIpcResponsePayload::Relation {
                item: handle.upsert_relation(draft).await?,
            })
        }
        MemoryIpcRequestPayload::DeleteRelation { relation_id } => {
            handle.delete_relation(relation_id).await?;
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::UpdateInjection {
            level,
            target_id,
            content,
        } => {
            handle.update_injection(level, target_id, content);
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::RunMicroRumination { turn_result } => {
            handle.run_micro_rumination(turn_result);
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::SubmitCandidate { candidate } => {
            handle.submit_memory_candidate(candidate);
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::RunEnhancedMicroRumination { turn_result } => {
            handle.run_enhanced_micro_rumination(turn_result).await;
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::RunMesoRumination {
            session_id,
            workspace_id,
        } => {
            handle.run_meso_rumination(session_id, workspace_id);
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::RunMetaRumination => {
            handle.run_meta_rumination();
            Ok(MemoryIpcResponsePayload::Ack)
        }
        MemoryIpcRequestPayload::Shutdown => {
            handle.shutdown().await;
            Ok(MemoryIpcResponsePayload::Ack)
        }
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

    pub async fn write_request(&mut self, request: IpcRequest) -> Result<()> {
        self.write_frame(&IpcFrame::Request(request)).await
    }

    pub async fn read_request(&mut self) -> Result<IpcRequest> {
        match self.read_frame().await? {
            IpcFrame::Request(req) => Ok(req),
            IpcFrame::Error { message } => bail!("IPC 对端返回错误: {message}"),
            frame => bail!("期望 Request 帧，实际收到: {:?}", frame),
        }
    }

    pub async fn write_response(&mut self, response: IpcResponse) -> Result<()> {
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

pub fn endpoint_path(service: &str) -> Result<PathBuf> {
    let base = runtime_dir()?;
    Ok(base.join(format!("{service}.json")))
}

pub fn load_endpoint(service: &str) -> Result<IpcEndpoint> {
    let path = endpoint_path(service)?;
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("读取 IPC endpoint 文件失败: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("解析 IPC endpoint 文件失败: {}", path.display()))
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

fn runtime_dir() -> Result<PathBuf> {
    Ok(home_dir()
        .ok_or_else(|| anyhow!("无法确定 HOME/USERPROFILE"))?
        .join(".tiangong")
        .join("memory")
        .join("runtime"))
}

fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard {
        prev_home: Option<std::ffi::OsString>,
        prev_userprofile: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn enter(home: &std::path::Path) -> Self {
            let prev_home = std::env::var_os("HOME");
            let prev_userprofile = std::env::var_os("USERPROFILE");
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("USERPROFILE", home);
            }
            Self {
                prev_home,
                prev_userprofile,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev_home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match &self.prev_userprofile {
                    Some(value) => std::env::set_var("USERPROFILE", value),
                    None => std::env::remove_var("USERPROFILE"),
                }
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn tcp_loopback_ipc_roundtrip_works() {
        let home = TempDir::new().expect("创建 fake home 失败");
        let _env = EnvGuard::enter(home.path());

        let server = IpcServer::bind("memory-test").await.expect("绑定 IPC 失败");
        let endpoint = load_endpoint("memory-test").expect("读取 endpoint 失败");

        let server_task = tokio::spawn(async move {
            let mut conn = server
                .accept_authenticated()
                .await
                .expect("服务端接受连接失败");
            let req = conn.read_request().await.expect("服务端读取请求失败");
            assert_eq!(req.request_id, "req-1");
            conn.write_response(IpcResponse {
                request_id: req.request_id,
                payload: serde_json::json!({ "ok": true }),
            })
            .await
            .expect("服务端写响应失败");
        });

        let mut client = IpcClient::connect_endpoint(&endpoint)
            .await
            .expect("客户端连接失败");
        client
            .send_request(IpcRequest {
                request_id: "req-1".to_string(),
                payload: serde_json::json!({ "hello": "world" }),
            })
            .await
            .expect("客户端写请求失败");
        let resp = client.read_response().await.expect("客户端读响应失败");
        assert_eq!(resp.request_id, "req-1");
        assert_eq!(resp.payload["ok"], true);

        server_task.await.expect("服务端任务失败");
    }
}
