//! Agent 的唯一物理命令通道与常驻 Driver 调度。
//!
//! 每个 session 只有一个 `Sender/Receiver`。所有外部输入都按 FIFO 进入该通道，
//! 唯一 Driver 在空闲、模型请求、工具执行与审批等待期间持续消费同一个 Receiver。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use futures_util::FutureExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::core::command::Command;

/// 唯一命令通道的发送端。所有 clone 都指向同一个物理通道。
#[derive(Clone)]
pub(crate) struct CommandIngress {
    accepting: Arc<std::sync::atomic::AtomicBool>,
    sender: UnboundedSender<Command>,
}

impl CommandIngress {
    fn new(
        accepting: Arc<std::sync::atomic::AtomicBool>,
        sender: UnboundedSender<Command>,
    ) -> Self {
        Self { accepting, sender }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(sender: UnboundedSender<Command>) -> Self {
        Self::new(Arc::new(std::sync::atomic::AtomicBool::new(true)), sender)
    }

    pub(crate) fn send(&self, command: Command) -> bool {
        use std::sync::atomic::Ordering;
        self.accepting.load(Ordering::Acquire) && self.sender.send(command).is_ok()
    }

    fn force_send(&self, command: Command) -> bool {
        self.sender.send(command).is_ok()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    pub(crate) fn is_accepting(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.accepting.load(Ordering::Acquire) && !self.sender.is_closed()
    }
}

struct SchedulingState {
    running: bool,
    close_error: Option<String>,
    driver: Option<JoinHandle<()>>,
}

/// 单个 Agent 的调度实体：唯一通道、唯一 Receiver 和唯一 Driver。
pub(crate) struct AgentScheduling {
    accepting: Arc<std::sync::atomic::AtomicBool>,
    ingress: CommandIngress,
    receiver: Mutex<Option<UnboundedReceiver<Command>>>,
    state: Mutex<SchedulingState>,
}

impl AgentScheduling {
    fn new() -> Self {
        let accepting = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (sender, receiver) = unbounded_channel();
        let ingress = CommandIngress::new(accepting.clone(), sender);
        Self {
            accepting,
            ingress,
            receiver: Mutex::new(Some(receiver)),
            state: Mutex::new(SchedulingState {
                running: false,
                close_error: None,
                driver: None,
            }),
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SchedulingState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn ingress(&self) -> CommandIngress {
        self.ingress.clone()
    }

    pub(crate) fn deliver(&self, command: Command) -> Result<(), crate::core::CoreError> {
        let marks_running = matches!(
            command,
            Command::InjectUserMessage { .. } | Command::CompressContext
        );
        let mut state = self.lock_state();
        if self.ingress.send(command) {
            if marks_running {
                state.running = true;
            }
            Ok(())
        } else {
            Err(crate::core::CoreError::WorkerStopped)
        }
    }

    pub(crate) fn take_receiver(&self) -> UnboundedReceiver<Command> {
        self.receiver
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .expect("唯一 Driver 只能领取一次命令 Receiver")
    }

    /// Driver 在准备阻塞前原子检查通道并完成状态收尾。
    /// 若已有下一条命令则保持 Running；若通道为空，则先切换 Idle，再发布上一活动的终态。
    /// `deliver` 与这里共用状态锁，确保终态发布和后续输入之间存在唯一顺序。
    pub(crate) fn try_recv_or_finish(
        &self,
        receiver: &mut UnboundedReceiver<Command>,
        terminal_event: &mut Option<tiangong_types::StreamEvent>,
        stream_tx: &std::sync::mpsc::Sender<tiangong_types::StreamEvent>,
    ) -> Result<Option<Command>, tokio::sync::mpsc::error::TryRecvError> {
        let mut state = self.lock_state();
        match receiver.try_recv() {
            Ok(command) => Ok(Some(command)),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                state.running = false;
                if let Some(event) = terminal_event.take() {
                    let _ = stream_tx.send(event);
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.lock_state().running
    }

    pub(crate) fn is_accepting(&self) -> bool {
        self.ingress.is_accepting()
    }

    pub(crate) fn set_close_error(&self, error: String) {
        self.lock_state().close_error = Some(error);
    }

    fn take_close_error(&self) -> Option<String> {
        self.lock_state().close_error.take()
    }

    fn shutdown(&self) -> Option<JoinHandle<()>> {
        use std::sync::atomic::Ordering;
        self.accepting.store(false, Ordering::Release);
        let _ = self.ingress.force_send(Command::Shutdown);
        self.lock_state().driver.take()
    }
}

type AgentMap = HashMap<String, Arc<AgentScheduling>>;
static AGENTS: OnceLock<Mutex<AgentMap>> = OnceLock::new();

fn agents() -> &'static Mutex<AgentMap> {
    AGENTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 确保 session 的唯一通道与唯一 Driver 存在。
pub(crate) fn ensure_agent_session<F, Fut>(
    session_id: &str,
    driver_factory: F,
) -> Result<Arc<AgentScheduling>, crate::core::CoreError>
where
    F: FnOnce(Arc<AgentScheduling>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut registry = agents()
        .lock()
        .map_err(|_| crate::core::CoreError::WorkerStopped)?;
    if let Some(entry) = registry.get(session_id) {
        if !entry.is_accepting() {
            return Err(crate::core::CoreError::WorkerStopped);
        }
        return Ok(entry.clone());
    }

    let entry = Arc::new(AgentScheduling::new());
    registry.insert(session_id.to_string(), entry.clone());
    let future = driver_factory(entry.clone());
    let panic_sid = session_id.to_string();
    let driver = crate::shared_runtime::shared_runtime().spawn(async move {
        let outcome = std::panic::AssertUnwindSafe(future).catch_unwind().await;
        if let Err(panic) = outcome {
            let detail = panic
                .downcast_ref::<&str>()
                .map(|value| value.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "未知 panic".to_string());
            tracing::error!(session_id = %panic_sid, detail, "Agent Driver 异常退出");
            remove_agent(&panic_sid);
        }
    });
    entry.lock_state().driver = Some(driver);
    Ok(entry)
}

pub(crate) fn remove_agent(session_id: &str) {
    if let Ok(mut registry) = agents().lock() {
        registry.remove(session_id);
    }
}

/// 向唯一物理通道投递命令。
pub fn send_command(session_id: &str, command: Command) -> bool {
    if let Ok(registry) = agents().lock()
        && let Some(entry) = registry.get(session_id)
    {
        return entry.ingress.send(command);
    }
    false
}

pub fn is_running(session_id: &str) -> bool {
    agents()
        .lock()
        .ok()
        .and_then(|registry| registry.get(session_id).cloned())
        .is_some_and(|entry| entry.is_running())
}

pub fn is_alive(session_id: &str) -> bool {
    agents()
        .lock()
        .ok()
        .and_then(|registry| registry.get(session_id).cloned())
        .is_some_and(|entry| entry.is_accepting())
}

pub fn shutdown_agent(session_id: &str) -> Result<(), crate::core::CoreError> {
    let entry = {
        let mut registry = agents()
            .lock()
            .map_err(|_| crate::core::CoreError::WorkerStopped)?;
        let Some(entry) = registry.remove(session_id) else {
            return Ok(());
        };
        entry
    };
    if let Some(driver) = entry.shutdown() {
        std::thread::scope(|scope| {
            scope
                .spawn(move || crate::shared_runtime::shared_runtime().block_on(driver))
                .join()
                .map_err(|_| crate::core::CoreError::WorkerPanicked)?
                .map_err(|_| crate::core::CoreError::WorkerPanicked)
        })?;
    }
    if let Some(error) = entry.take_close_error() {
        tracing::warn!(%error, session_id, "关闭时持久化未处理消息失败");
        return Err(crate::core::CoreError::WorkerStopped);
    }
    Ok(())
}

pub fn detach_shutdown(session_id: &str) {
    let driver = {
        let Ok(mut registry) = agents().lock() else {
            return;
        };
        let Some(entry) = registry.remove(session_id) else {
            return;
        };
        entry.shutdown()
    };
    drop(driver);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_channel_preserves_fifo_and_never_returns_busy() {
        let sid = format!("single-channel-{}", scru128::new_string());
        let entry = ensure_agent_session(&sid, |entry| async move {
            let mut receiver = entry.take_receiver();
            let first = receiver.recv().await.expect("第一条命令");
            let second = receiver.recv().await.expect("第二条命令");
            assert!(
                matches!(first, Command::InjectUserMessage { message_id, .. } if message_id == "a")
            );
            assert!(
                matches!(second, Command::InjectUserMessage { message_id, .. } if message_id == "b")
            );
        })
        .expect("创建 Agent");
        entry
            .deliver(Command::InjectUserMessage {
                message_id: "a".to_string(),
                content: vec![],
            })
            .expect("第一条消息");
        entry
            .deliver(Command::InjectUserMessage {
                message_id: "b".to_string(),
                content: vec![],
            })
            .expect("第二条消息不应 Busy");
        shutdown_agent(&sid).expect("关闭 Agent");
    }

    #[test]
    fn running_stays_true_until_no_active_or_queued_user_work_remains() {
        let entry = AgentScheduling::new();
        let mut receiver = entry.take_receiver();
        let (stream_tx, stream_rx) = std::sync::mpsc::channel();
        let mut terminal_event = None;

        entry
            .deliver(Command::InjectUserMessage {
                message_id: "first".to_string(),
                content: vec![],
            })
            .expect("首条消息应投递成功");
        assert!(entry.is_running(), "消息进入唯一通道后应立即视为运行中");
        assert!(matches!(
            entry.try_recv_or_finish(&mut receiver, &mut terminal_event, &stream_tx),
            Ok(Some(Command::InjectUserMessage { message_id, .. })) if message_id == "first"
        ));

        entry
            .deliver(Command::InjectUserMessage {
                message_id: "second".to_string(),
                content: vec![],
            })
            .expect("连续消息应投递成功");
        assert!(matches!(
            entry.try_recv_or_finish(&mut receiver, &mut terminal_event, &stream_tx),
            Ok(Some(Command::InjectUserMessage { message_id, .. })) if message_id == "second"
        ));
        assert!(entry.is_running(), "领取后续消息时不得短暂切换为空闲");

        terminal_event = Some(tiangong_types::StreamEvent::Done { usage: None });
        assert!(matches!(
            entry.try_recv_or_finish(&mut receiver, &mut terminal_event, &stream_tx),
            Ok(None)
        ));
        assert!(!entry.is_running(), "活动和通道都为空时才应进入空闲");
        assert!(matches!(
            stream_rx.try_recv(),
            Ok(tiangong_types::StreamEvent::Done { .. })
        ));
    }
}
