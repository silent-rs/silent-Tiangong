use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tiangong_scheduler::executor::SchedulerContext;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

/// 定时消息投递请求。
///
/// Desktop 调度器只负责把请求交回 [`crate::app::TiangongApp`]；消息归档、Core
/// 复用及流事件消费全部走桌面端统一链路，避免同一会话出现第二个 Core。
pub(crate) struct ScheduledMessageRequest {
    pub(crate) session_id: String,
    pub(crate) content: String,
    pub(crate) stable_enqueue_ack: oneshot::Sender<Result<(), String>>,
}

/// Desktop 端调度器执行上下文。
///
/// 这里不持有 Core，也不消费 Core 事件流。定时消息经通道投递给 TiangongApp，
/// 并只等待消息稳定入队，不等待整个模型轮次完成。
pub struct DesktopSchedulerContext {
    state: Arc<AsyncMutex<tiangong_app_state::app_state::TiangongState>>,
    scheduled_message_tx: mpsc::UnboundedSender<ScheduledMessageRequest>,
    scheduled_session_locks: ScheduledSessionLocks,
}

#[derive(Default)]
struct ScheduledSessionLocks {
    by_session: std::sync::Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl ScheduledSessionLocks {
    fn lock_for(&self, session_id: &str) -> Arc<AsyncMutex<()>> {
        self.by_session
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
}

impl DesktopSchedulerContext {
    pub(crate) fn new(
        state: Arc<AsyncMutex<tiangong_app_state::app_state::TiangongState>>,
        scheduled_message_tx: mpsc::UnboundedSender<ScheduledMessageRequest>,
    ) -> Self {
        Self {
            state,
            scheduled_message_tx,
            scheduled_session_locks: ScheduledSessionLocks::default(),
        }
    }
}

#[async_trait]
impl SchedulerContext for DesktopSchedulerContext {
    async fn send_message(&self, session_id: &str, content: String) -> anyhow::Result<()> {
        enqueue_scheduled_message(
            &self.scheduled_message_tx,
            &self.scheduled_session_locks,
            session_id,
            content,
        )
        .await
    }

    async fn resolve_or_create_session(
        &self,
        requested_session_id: Option<&str>,
        trigger_name: &str,
    ) -> anyhow::Result<(String, bool)> {
        if let Some(sid) = requested_session_id {
            let state = self.state.lock().await;
            if state.sessions().iter().any(|s| s.id == *sid) {
                return Ok((sid.to_string(), false));
            }
        }

        let mut state = self.state.lock().await;
        let title = format!("定时任务：{}", trigger_name);
        let session = tiangong_core::session::Session::new_isolated(
            title,
            &tiangong_app_state::app_state::storage_root(),
        );
        let session_id = session.id.clone();
        state.sessions_mut().push(session);
        state.persist_session(&session_id)?;
        Ok((session_id, true))
    }
}

async fn enqueue_scheduled_message(
    sender: &mpsc::UnboundedSender<ScheduledMessageRequest>,
    session_locks: &ScheduledSessionLocks,
    session_id: &str,
    content: String,
) -> anyhow::Result<()> {
    // 同一会话从进入通道直到稳定 ACK 保持 FIFO；不同会话使用不同锁，可并行投递。
    let session_lock = session_locks.lock_for(session_id);
    let _session_guard = session_lock.lock_owned().await;
    let (ack_tx, ack_rx) = oneshot::channel();
    sender
        .send(ScheduledMessageRequest {
            session_id: session_id.to_string(),
            content,
            stable_enqueue_ack: ack_tx,
        })
        .map_err(|_| anyhow::anyhow!("桌面端定时消息路由已关闭"))?;

    match ack_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(anyhow::anyhow!(error)),
        Err(_) => Err(anyhow::anyhow!("桌面端定时消息路由未返回入队确认")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scheduled_message_waits_for_stable_enqueue_ack() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let locks = Arc::new(ScheduledSessionLocks::default());
        let locks_in_send = locks.clone();
        let mut send = tokio::spawn(async move {
            enqueue_scheduled_message(&tx, &locks_in_send, "session-1", "run now".to_string()).await
        });

        let request = rx.recv().await.expect("定时消息应进入统一路由");
        assert_eq!(request.session_id, "session-1");
        assert_eq!(request.content, "run now");
        tokio::task::yield_now().await;
        assert!(!send.is_finished(), "稳定入队确认前不应提前返回");

        request
            .stable_enqueue_ack
            .send(Ok(()))
            .expect("发送方应仍在等待确认");
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut send)
            .await
            .expect("确认后应立即返回，不等待模型轮次")
            .expect("发送任务不应失败")
            .expect("稳定入队应成功");
    }

    #[tokio::test]
    async fn scheduled_message_propagates_enqueue_failure() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let locks = Arc::new(ScheduledSessionLocks::default());
        let send = tokio::spawn(async move {
            enqueue_scheduled_message(&tx, &locks, "session-1", "run now".to_string()).await
        });

        let request = rx.recv().await.expect("定时消息应进入统一路由");
        request
            .stable_enqueue_ack
            .send(Err("persist failed".to_string()))
            .expect("发送方应仍在等待确认");
        let error = send
            .await
            .expect("发送任务不应 panic")
            .expect_err("入队失败必须返回给调度器");
        assert!(error.to_string().contains("persist failed"));
    }

    #[tokio::test]
    async fn same_session_requests_remain_fifo_until_ack() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let locks = Arc::new(ScheduledSessionLocks::default());
        let first_tx = tx.clone();
        let first_locks = locks.clone();
        let first = tokio::spawn(async move {
            enqueue_scheduled_message(&first_tx, &first_locks, "session-1", "first".to_string())
                .await
        });
        let first_request = rx.recv().await.expect("第一条消息应先进入路由");

        let second_locks = locks.clone();
        let second = tokio::spawn(async move {
            enqueue_scheduled_message(&tx, &second_locks, "session-1", "second".to_string()).await
        });
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err(), "同会话第二条消息必须等待第一条 ACK");

        first_request
            .stable_enqueue_ack
            .send(Ok(()))
            .expect("第一条发送方应等待 ACK");
        let second_request = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("第一条 ACK 后第二条应继续")
            .expect("第二条消息应进入路由");
        assert_eq!(second_request.content, "second");
        second_request
            .stable_enqueue_ack
            .send(Ok(()))
            .expect("第二条发送方应等待 ACK");
        first.await.expect("第一条任务不应 panic").unwrap();
        second.await.expect("第二条任务不应 panic").unwrap();
    }

    #[tokio::test]
    async fn different_sessions_can_wait_for_ack_in_parallel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let locks = Arc::new(ScheduledSessionLocks::default());
        let first_tx = tx.clone();
        let first_locks = locks.clone();
        let first = tokio::spawn(async move {
            enqueue_scheduled_message(&first_tx, &first_locks, "session-1", "first".to_string())
                .await
        });
        let first_request = rx.recv().await.expect("第一会话消息应进入路由");

        let second_locks = locks.clone();
        let second = tokio::spawn(async move {
            enqueue_scheduled_message(&tx, &second_locks, "session-2", "second".to_string()).await
        });
        let second_request = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("不同会话不应被第一会话 ACK 阻塞")
            .expect("第二会话消息应进入路由");
        assert_eq!(second_request.session_id, "session-2");

        first_request.stable_enqueue_ack.send(Ok(())).unwrap();
        second_request.stable_enqueue_ack.send(Ok(())).unwrap();
        first.await.expect("第一会话任务不应 panic").unwrap();
        second.await.expect("第二会话任务不应 panic").unwrap();
    }
}
