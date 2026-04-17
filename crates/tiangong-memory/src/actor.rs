//! Memory Actor — 独立 tokio task 运行时
//!
//! 所有 Memory 读写均串行经过 Actor，天然消除并发冲突。
//! 外部通过 MemoryHandle 的 mpsc channel 发送命令。

use tokio::sync::mpsc;

use crate::command::MemoryCommand;
use crate::handle::MemoryHandle;
use crate::rumination;
use crate::store::MemoryStore;

/// Memory Actor（独立运行时）
pub(crate) struct MemoryActor {
    rx: mpsc::Receiver<MemoryCommand>,
    store: MemoryStore,
    workspace_id: Option<String>,
}

impl MemoryActor {
    pub(crate) fn new(
        rx: mpsc::Receiver<MemoryCommand>,
        store: MemoryStore,
        workspace_id: Option<String>,
    ) -> Self {
        Self {
            rx,
            store,
            workspace_id,
        }
    }

    /// 启动 Actor 消息循环
    pub(crate) async fn run(mut self) {
        tracing::info!("Memory Actor 已启动");
        loop {
            match self.rx.recv().await {
                Some(MemoryCommand::Shutdown) => {
                    tracing::info!("Memory Actor 收到 Shutdown 命令，正在关闭");
                    break;
                }
                Some(cmd) => self.handle(cmd).await,
                None => {
                    tracing::info!("Memory Actor 通道已关闭，退出");
                    break;
                }
            }
        }
        tracing::info!("Memory Actor 已关闭");
    }

    async fn handle(&mut self, cmd: MemoryCommand) {
        match cmd {
            MemoryCommand::LoadInjection {
                session_id,
                workspace_id,
                reply,
            } => {
                let result = self
                    .store
                    .load_injection(&session_id, workspace_id.as_deref());
                let _ = reply.send(result);
            }

            MemoryCommand::WriteEpisode {
                episode,
                workspace_id,
            } => {
                if let Err(e) = self.store.write_episode(episode, workspace_id.as_deref()) {
                    tracing::warn!("Memory 写入 Episode 失败: {}", e);
                }
            }

            MemoryCommand::UpdateInjection {
                level,
                target_id,
                content,
            } => {
                if let Err(e) = self.store.update_injection(level, &target_id, &content) {
                    tracing::warn!("Memory 更新 Injection 失败: {}", e);
                }
            }

            // Phase C：双引擎召回（Tantivy BM25 + Qdrant 语义）
            MemoryCommand::Recall {
                anchors,
                limit,
                reply,
            } => {
                let hits = self.store.recall_async(&anchors, limit).await;
                let _ = reply.send(hits);
            }

            // Phase C 实现
            MemoryCommand::LoadDepth2 { reply, .. } => {
                let _ = reply.send(Vec::new());
            }

            // Phase B：Micro 反刍
            MemoryCommand::RunMicroRumination { turn_result } => {
                // 优先使用 turn 携带的 workspace_id，避免跨工作区串写
                let wid = turn_result
                    .workspace_id
                    .as_deref()
                    .or(self.workspace_id.as_deref());
                if let Err(e) = rumination::process_micro(&mut self.store, &turn_result, wid) {
                    tracing::warn!("Micro 反刍失败: {}", e);
                }
            }

            // Phase C 实现
            MemoryCommand::RunMesoRumination {
                session_id,
                workspace_id,
            } => {
                if let Err(e) =
                    rumination::process_meso(&mut self.store, &session_id, &workspace_id)
                {
                    tracing::warn!("Meso 反刍失败: {}", e);
                }
            }

            // Phase D 实现
            MemoryCommand::RunMetaRumination => {
                if let Err(e) = rumination::process_meta(&mut self.store) {
                    tracing::warn!("Meta 反刍失败: {}", e);
                }
            }

            // Shutdown 在外层 loop 处理
            MemoryCommand::Shutdown => {}
        }
    }
}

/// 启动 Memory 系统，返回 MemoryHandle（同步接口）
///
/// 内部在独立线程 + current_thread runtime + LocalSet 中运行 Actor，
/// 不要求 MemoryActor 或 MemoryStore 实现 Send/Sync。
pub fn start_memory(workspace_id: Option<String>) -> anyhow::Result<MemoryHandle> {
    let (tx, rx) = mpsc::channel(256);

    let store = MemoryStore::open(workspace_id.clone()).map_err(|e| {
        tracing::error!("Memory Store 初始化失败: {}", e);
        e
    })?;

    let actor = MemoryActor::new(rx, store, workspace_id);

    std::thread::Builder::new()
        .name("tiangong-memory-actor".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Memory Actor runtime 构建失败");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, actor.run());
        })
        .expect("Memory Actor 线程创建失败");

    Ok(MemoryHandle::new(tx))
}
