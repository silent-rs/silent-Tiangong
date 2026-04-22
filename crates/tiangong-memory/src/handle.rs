//! Memory 系统客户端句柄
//!
//! 可任意 Clone 跨线程/任务使用，通过 mpsc channel 与 MemoryActor 通信。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use crate::command::{InjectionLevel, MemoryCommand};
use crate::ipc::IpcClient;
use crate::ipc::protocol::{IpcRequest, MemoryIpcRequestPayload, MemoryIpcResponsePayload};
use crate::options::MemoryOptions;
use crate::types::{
    Episode, ExpandedMemory, MemoryRecallRequest, MemoryRecallResponse, RecallAnchors, RecallHit,
    TurnResult,
};

/// Memory 系统的客户端句柄，可任意 Clone 跨线程使用
#[derive(Clone)]
pub struct MemoryHandle {
    inner: Arc<HandleInner>,
}

enum HandleInner {
    Local { tx: mpsc::Sender<MemoryCommand> },
    Remote { client: Mutex<IpcClient> },
}

impl MemoryHandle {
    pub(crate) fn new(tx: mpsc::Sender<MemoryCommand>) -> Self {
        Self {
            inner: Arc::new(HandleInner::Local { tx }),
        }
    }

    pub async fn connect_tcp(service: &str) -> Result<Self> {
        let client = IpcClient::connect(service).await?;
        Ok(Self {
            inner: Arc::new(HandleInner::Remote {
                client: Mutex::new(client),
            }),
        })
    }

    /// 判断两个 handle 是否指向同一个底层 handle 实例。
    ///
    /// 主要用于 registry 生命周期测试和诊断：同一 workspace 的重复获取应返回
    /// 同一个 handle clone；不同 workspace 应创建不同 handle。
    pub fn is_same_handle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// 同步热更新 Memory Actor 配置。
    ///
    /// 该方法用于 Core 工作线程响应配置 generation 变化；远程 handle 暂不支持
    /// 跨进程重配置，调用方应在本进程 registry 内使用。
    pub fn reconfigure_blocking(&self, options: MemoryOptions) -> Result<()> {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                tx.blocking_send(MemoryCommand::Reconfigure {
                    options: Box::new(options),
                    reply: reply_tx,
                })
                .with_context(|| "发送 Memory 热更新命令失败")?;
                reply_rx
                    .recv_timeout(Duration::from_secs(30))
                    .with_context(|| "等待 Memory 热更新结果超时或失败")?
                    .map_err(|err| anyhow!(err))
            }
            HandleInner::Remote { .. } => Err(anyhow!("远程 MemoryHandle 暂不支持配置热更新")),
        }
    }

    /// 加载注入上下文（查询，等待响应）
    pub async fn load_injection(
        &self,
        session_id: &str,
        workspace_id: Option<&str>,
    ) -> Vec<String> {
        match self.inner.as_ref() {
            HandleInner::Local { .. } => self.load_injection_local(session_id, workspace_id).await,
            HandleInner::Remote { .. } => {
                match self
                    .send_remote_request(MemoryIpcRequestPayload::LoadInjection {
                        session_id: session_id.to_string(),
                        workspace_id: workspace_id.map(String::from),
                    })
                    .await
                {
                    Ok(MemoryIpcResponsePayload::Injection { items }) => items,
                    Ok(other) => {
                        tracing::warn!("Memory IPC load_injection 返回了非预期响应: {:?}", other);
                        Vec::new()
                    }
                    Err(e) => {
                        tracing::warn!("Memory IPC load_injection 失败: {}", e);
                        Vec::new()
                    }
                }
            }
        }
    }

    /// 执行粗召回（查询，等待响应）
    pub async fn recall(&self, anchors: RecallAnchors, limit: usize) -> Vec<RecallHit> {
        match self.inner.as_ref() {
            HandleInner::Local { .. } => self.recall_local(anchors, limit).await,
            HandleInner::Remote { .. } => {
                match self
                    .send_remote_request(MemoryIpcRequestPayload::Recall { anchors, limit })
                    .await
                {
                    Ok(MemoryIpcResponsePayload::Recall { hits }) => hits,
                    Ok(other) => {
                        tracing::warn!("Memory IPC recall 返回了非预期响应: {:?}", other);
                        Vec::new()
                    }
                    Err(e) => {
                        tracing::warn!("Memory IPC recall 失败: {}", e);
                        Vec::new()
                    }
                }
            }
        }
    }

    /// 执行 Tool 化上下文回忆，由 Memory 内部完成检索规划和结果整理。
    pub async fn recall_context(&self, request: MemoryRecallRequest) -> MemoryRecallResponse {
        match self.inner.as_ref() {
            HandleInner::Local { .. } => self.recall_context_local(request).await,
            HandleInner::Remote { .. } => {
                match self
                    .send_remote_request(MemoryIpcRequestPayload::RecallContext { request })
                    .await
                {
                    Ok(MemoryIpcResponsePayload::RecallContext { response }) => response,
                    Ok(other) => {
                        tracing::warn!("Memory IPC recall_context 返回了非预期响应: {:?}", other);
                        MemoryRecallResponse::default()
                    }
                    Err(e) => {
                        tracing::warn!("Memory IPC recall_context 失败: {}", e);
                        MemoryRecallResponse::default()
                    }
                }
            }
        }
    }

    /// 写入 Episode（fire-and-forget）
    ///
    /// `workspace_id` 显式携带，为 `None` 时由 Actor 内部值兜底。
    pub fn write_episode(&self, episode: Episode, workspace_id: Option<String>) {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                if let Err(e) = tx.try_send(MemoryCommand::WriteEpisode {
                    episode,
                    workspace_id,
                }) {
                    tracing::warn!("Memory write_episode 发送失败: {}", e);
                }
            }
            HandleInner::Remote { .. } => {
                self.dispatch_remote_request(
                    MemoryIpcRequestPayload::WriteEpisode {
                        episode,
                        workspace_id,
                    },
                    "write_episode",
                );
            }
        }
    }

    /// 更新注入文件（fire-and-forget）
    pub fn update_injection(&self, level: InjectionLevel, target_id: String, content: String) {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                if let Err(e) = tx.try_send(MemoryCommand::UpdateInjection {
                    level,
                    target_id,
                    content,
                }) {
                    tracing::warn!("Memory update_injection 发送失败: {}", e);
                }
            }
            HandleInner::Remote { .. } => {
                self.dispatch_remote_request(
                    MemoryIpcRequestPayload::UpdateInjection {
                        level,
                        target_id,
                        content,
                    },
                    "update_injection",
                );
            }
        }
    }

    /// 触发 Micro 反刍（fire-and-forget）
    pub fn run_micro_rumination(&self, turn_result: TurnResult) {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                if let Err(e) = tx.try_send(MemoryCommand::RunMicroRumination {
                    turn_result: Box::new(turn_result),
                }) {
                    tracing::warn!("Memory run_micro_rumination 发送失败: {}", e);
                }
            }
            HandleInner::Remote { .. } => {
                self.dispatch_remote_request(
                    MemoryIpcRequestPayload::RunMicroRumination { turn_result },
                    "run_micro_rumination",
                );
            }
        }
    }

    /// 触发 Micro 反刍（同步版，适用于 std::thread 中的 blocking_send）
    ///
    /// 在非 async 上下文（如 TiangongCore 工作线程）中使用。
    pub fn run_micro_rumination_blocking(&self, turn_result: TurnResult) {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                if let Err(e) = tx.blocking_send(MemoryCommand::RunMicroRumination {
                    turn_result: Box::new(turn_result),
                }) {
                    tracing::warn!("Memory run_micro_rumination_blocking 发送失败: {}", e);
                }
            }
            HandleInner::Remote { .. } => {
                if let Err(e) =
                    self.block_on_remote_request(MemoryIpcRequestPayload::RunMicroRumination {
                        turn_result,
                    })
                {
                    tracing::warn!("Memory run_micro_rumination_blocking 远端发送失败: {}", e);
                }
            }
        }
    }

    /// 执行粗召回（同步版，适用于 std::thread 中使用）
    ///
    /// 在非 async 上下文（如 TiangongCore 工作线程）中使用。
    pub fn recall_blocking(&self, anchors: RecallAnchors, limit: usize) -> Vec<RecallHit> {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let cmd = MemoryCommand::Recall {
                    anchors,
                    limit,
                    reply: reply_tx,
                };
                if tx.blocking_send(cmd).is_err() {
                    tracing::warn!("Memory Actor 已关闭，返回空召回");
                    return Vec::new();
                }
                reply_rx.blocking_recv().unwrap_or_default()
            }
            HandleInner::Remote { .. } => match self
                .block_on_remote_request(MemoryIpcRequestPayload::Recall { anchors, limit })
            {
                Ok(MemoryIpcResponsePayload::Recall { hits }) => hits,
                Ok(other) => {
                    tracing::warn!("Memory IPC recall_blocking 返回了非预期响应: {:?}", other);
                    Vec::new()
                }
                Err(e) => {
                    tracing::warn!("Memory IPC recall_blocking 失败: {}", e);
                    Vec::new()
                }
            },
        }
    }

    /// Tool 化上下文回忆（同步版，适用于 std::thread 中使用）。
    pub fn recall_context_blocking(&self, request: MemoryRecallRequest) -> MemoryRecallResponse {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let cmd = MemoryCommand::RecallContext {
                    request,
                    reply: reply_tx,
                };
                if tx.blocking_send(cmd).is_err() {
                    tracing::warn!("Memory Actor 已关闭，返回空上下文回忆");
                    return MemoryRecallResponse::default();
                }
                reply_rx.blocking_recv().unwrap_or_default()
            }
            HandleInner::Remote { .. } => match self
                .block_on_remote_request(MemoryIpcRequestPayload::RecallContext { request })
            {
                Ok(MemoryIpcResponsePayload::RecallContext { response }) => response,
                Ok(other) => {
                    tracing::warn!(
                        "Memory IPC recall_context_blocking 返回了非预期响应: {:?}",
                        other
                    );
                    MemoryRecallResponse::default()
                }
                Err(e) => {
                    tracing::warn!("Memory IPC recall_context_blocking 失败: {}", e);
                    MemoryRecallResponse::default()
                }
            },
        }
    }

    /// 加载二跳展开内容（查询，等待响应）
    pub async fn load_depth2(&self, node_ids: Vec<String>) -> Vec<ExpandedMemory> {
        match self.inner.as_ref() {
            HandleInner::Local { .. } => self.load_depth2_local(node_ids).await,
            HandleInner::Remote { .. } => {
                match self
                    .send_remote_request(MemoryIpcRequestPayload::LoadDepth2 { node_ids })
                    .await
                {
                    Ok(MemoryIpcResponsePayload::Depth2 { items }) => items,
                    Ok(other) => {
                        tracing::warn!("Memory IPC load_depth2 返回了非预期响应: {:?}", other);
                        Vec::new()
                    }
                    Err(e) => {
                        tracing::warn!("Memory IPC load_depth2 失败: {}", e);
                        Vec::new()
                    }
                }
            }
        }
    }

    /// 触发 Meso 反刍（fire-and-forget）
    pub fn run_meso_rumination(&self, session_id: String, workspace_id: String) {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                if let Err(e) = tx.try_send(MemoryCommand::RunMesoRumination {
                    session_id,
                    workspace_id,
                }) {
                    tracing::warn!("Memory run_meso_rumination 发送失败: {}", e);
                }
            }
            HandleInner::Remote { .. } => {
                self.dispatch_remote_request(
                    MemoryIpcRequestPayload::RunMesoRumination {
                        session_id,
                        workspace_id,
                    },
                    "run_meso_rumination",
                );
            }
        }
    }

    /// 触发 Meta 反刍（fire-and-forget）
    pub fn run_meta_rumination(&self) {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                if let Err(e) = tx.try_send(MemoryCommand::RunMetaRumination) {
                    tracing::warn!("Memory run_meta_rumination 发送失败: {}", e);
                }
            }
            HandleInner::Remote { .. } => {
                self.dispatch_remote_request(
                    MemoryIpcRequestPayload::RunMetaRumination,
                    "run_meta_rumination",
                );
            }
        }
    }

    /// 优雅关闭
    pub async fn shutdown(&self) {
        match self.inner.as_ref() {
            HandleInner::Local { tx } => {
                let _ = tx.send(MemoryCommand::Shutdown).await;
            }
            HandleInner::Remote { .. } => {
                let _ = self
                    .send_remote_request(MemoryIpcRequestPayload::Shutdown)
                    .await;
            }
        }
    }

    async fn load_injection_local(
        &self,
        session_id: &str,
        workspace_id: Option<&str>,
    ) -> Vec<String> {
        let HandleInner::Local { tx } = self.inner.as_ref() else {
            return Vec::new();
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = MemoryCommand::LoadInjection {
            session_id: session_id.to_string(),
            workspace_id: workspace_id.map(String::from),
            reply: reply_tx,
        };
        if tx.send(cmd).await.is_err() {
            tracing::warn!("Memory Actor 已关闭，返回空注入");
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    async fn recall_local(&self, anchors: RecallAnchors, limit: usize) -> Vec<RecallHit> {
        let HandleInner::Local { tx } = self.inner.as_ref() else {
            return Vec::new();
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = MemoryCommand::Recall {
            anchors,
            limit,
            reply: reply_tx,
        };
        if tx.send(cmd).await.is_err() {
            tracing::warn!("Memory Actor 已关闭，返回空召回");
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    async fn recall_context_local(&self, request: MemoryRecallRequest) -> MemoryRecallResponse {
        let HandleInner::Local { tx } = self.inner.as_ref() else {
            return MemoryRecallResponse::default();
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = MemoryCommand::RecallContext {
            request,
            reply: reply_tx,
        };
        if tx.send(cmd).await.is_err() {
            tracing::warn!("Memory Actor 已关闭，返回空上下文回忆");
            return MemoryRecallResponse::default();
        }
        reply_rx.await.unwrap_or_default()
    }

    async fn load_depth2_local(&self, node_ids: Vec<String>) -> Vec<ExpandedMemory> {
        let HandleInner::Local { tx } = self.inner.as_ref() else {
            return Vec::new();
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = MemoryCommand::LoadDepth2 {
            node_ids,
            reply: reply_tx,
        };
        if tx.send(cmd).await.is_err() {
            tracing::warn!("Memory Actor 已关闭，返回空 depth2");
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    async fn send_remote_request(
        &self,
        payload: MemoryIpcRequestPayload,
    ) -> Result<MemoryIpcResponsePayload> {
        let HandleInner::Remote { client } = self.inner.as_ref() else {
            return Err(anyhow!("当前 MemoryHandle 不是 Remote 模式"));
        };
        let mut client = client.lock().await;
        client
            .send_request(IpcRequest {
                request_id: scru128::new().to_string(),
                payload: serde_json::to_value(payload)
                    .with_context(|| "序列化 Memory IPC 请求载荷失败")?,
            })
            .await?;
        let response = client.read_response().await?;
        serde_json::from_value(response.payload).with_context(|| "解析 Memory IPC 响应载荷失败")
    }

    fn block_on_remote_request(
        &self,
        payload: MemoryIpcRequestPayload,
    ) -> Result<MemoryIpcResponsePayload> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .with_context(|| "构建临时 tokio runtime 失败")?;
        rt.block_on(self.send_remote_request(payload))
    }

    fn dispatch_remote_request(&self, payload: MemoryIpcRequestPayload, action: &'static str) {
        let handle = self.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(err) = handle.send_remote_request(payload).await {
                    tracing::warn!("Memory {action} 远端发送失败: {}", err);
                }
            });
            return;
        }

        let spawn_result = std::thread::Builder::new()
            .name(format!("memory-remote-{action}"))
            .spawn(move || {
                if let Err(err) = handle.block_on_remote_request(payload) {
                    tracing::warn!("Memory {action} 远端发送失败: {}", err);
                }
            });
        if let Err(err) = spawn_result {
            tracing::warn!("Memory {action} 启动远端发送线程失败: {}", err);
        }
    }
}
