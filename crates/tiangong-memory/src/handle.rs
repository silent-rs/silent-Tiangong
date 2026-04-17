//! Memory 系统客户端句柄
//!
//! 可任意 Clone 跨线程/任务使用，通过 mpsc channel 与 MemoryActor 通信。

use tokio::sync::mpsc;

use crate::command::{InjectionLevel, MemoryCommand};
use crate::types::{Episode, RecallAnchors, RecallHit, TurnResult};

/// Memory 系统的客户端句柄，可任意 Clone 跨线程使用
#[derive(Clone)]
pub struct MemoryHandle {
    tx: mpsc::Sender<MemoryCommand>,
}

impl MemoryHandle {
    pub(crate) fn new(tx: mpsc::Sender<MemoryCommand>) -> Self {
        Self { tx }
    }

    /// 加载注入上下文（查询，等待响应）
    pub async fn load_injection(
        &self,
        session_id: &str,
        workspace_id: Option<&str>,
    ) -> Vec<String> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = MemoryCommand::LoadInjection {
            session_id: session_id.to_string(),
            workspace_id: workspace_id.map(String::from),
            reply: reply_tx,
        };
        if self.tx.send(cmd).await.is_err() {
            tracing::warn!("Memory Actor 已关闭，返回空注入");
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    /// 执行粗召回（查询，等待响应）
    pub async fn recall(&self, anchors: RecallAnchors, limit: usize) -> Vec<RecallHit> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = MemoryCommand::Recall {
            anchors,
            limit,
            reply: reply_tx,
        };
        if self.tx.send(cmd).await.is_err() {
            tracing::warn!("Memory Actor 已关闭，返回空召回");
            return Vec::new();
        }
        reply_rx.await.unwrap_or_default()
    }

    /// 写入 Episode（fire-and-forget）
    ///
    /// `workspace_id` 显式携带，为 `None` 时由 Actor 内部值兜底。
    pub fn write_episode(&self, episode: Episode, workspace_id: Option<String>) {
        if let Err(e) = self.tx.try_send(MemoryCommand::WriteEpisode {
            episode,
            workspace_id,
        }) {
            tracing::warn!("Memory write_episode 发送失败: {}", e);
        }
    }

    /// 更新注入文件（fire-and-forget）
    pub fn update_injection(&self, level: InjectionLevel, target_id: String, content: String) {
        if let Err(e) = self.tx.try_send(MemoryCommand::UpdateInjection {
            level,
            target_id,
            content,
        }) {
            tracing::warn!("Memory update_injection 发送失败: {}", e);
        }
    }

    /// 触发 Micro 反刍（fire-and-forget）
    pub fn run_micro_rumination(&self, turn_result: TurnResult) {
        if let Err(e) = self.tx.try_send(MemoryCommand::RunMicroRumination {
            turn_result: Box::new(turn_result),
        }) {
            tracing::warn!("Memory run_micro_rumination 发送失败: {}", e);
        }
    }

    /// 触发 Micro 反刍（同步版，适用于 std::thread 中的 blocking_send）
    ///
    /// 在非 async 上下文（如 TiangongCore 工作线程）中使用。
    pub fn run_micro_rumination_blocking(&self, turn_result: TurnResult) {
        if let Err(e) = self.tx.blocking_send(MemoryCommand::RunMicroRumination {
            turn_result: Box::new(turn_result),
        }) {
            tracing::warn!("Memory run_micro_rumination_blocking 发送失败: {}", e);
        }
    }

    /// 执行粗召回（同步版，适用于 std::thread 中使用）
    ///
    /// 在非 async 上下文（如 TiangongCore 工作线程）中使用。
    pub fn recall_blocking(&self, anchors: RecallAnchors, limit: usize) -> Vec<RecallHit> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let cmd = MemoryCommand::Recall {
            anchors,
            limit,
            reply: reply_tx,
        };
        if self.tx.blocking_send(cmd).is_err() {
            tracing::warn!("Memory Actor 已关闭，返回空召回");
            return Vec::new();
        }
        reply_rx.blocking_recv().unwrap_or_default()
    }

    /// 优雅关闭
    pub async fn shutdown(&self) {
        let _ = self.tx.send(MemoryCommand::Shutdown).await;
    }
}
