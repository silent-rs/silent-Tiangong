//! 取消原语：ReAct 主循环与总结阶段共享的取消处理基础设施。
//!
//! `select!` 循环 break 时使用可被 anyhow downcast 的取消信号类型，替代裸字符串。

use std::future::Future;
use std::sync::mpsc::Sender as StdSender;

use tiangong_types::StreamEvent;
use tokio::sync::oneshot;

use crate::react::context::emit_token_usage;

/// select! 循环 break 时携带的取消信号，替代魔术字符串。
#[derive(Clone, Copy)]
pub(super) enum CancelSignal {
    /// 直接 abort LLM future，上报已累积的 streaming_usage
    Abort,
}

impl CancelSignal {
    /// 从 anyhow::Error 中提取 CancelSignal，如果不是取消信号返回 None。
    pub(super) fn from_error(err: &anyhow::Error) -> Option<Self> {
        err.downcast_ref::<Self>().copied()
    }
}

impl std::fmt::Display for CancelSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancelSignal::Abort => write!(f, "cancelled (abort)"),
        }
    }
}

impl std::fmt::Debug for CancelSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for CancelSignal {}

/// 取消时上报尚未发送的 token usage；终态由 run_turn 统一发布。
///
/// `usage` 只允许传入尚未通过其他 `TokenUsage` 事件上报的当前请求增量；此前轮次
/// 已上报的累计值不得再次放入取消事件，否则 Core 会重复记账。
pub(super) fn emit_cancel_usage(
    stream_tx: &StdSender<StreamEvent>,
    usage: &tiangong_types::TokenUsage,
    context_limit: usize,
) {
    if usage.total_tokens > 0 {
        emit_token_usage(
            stream_tx,
            usage,
            None,
            context_limit,
            "cancelled-incremental",
            None,
        );
    }
}

/// 请求取消异步 LLM 任务并等待其真正退出，确保它不再越过轮次屏障发送迟到事件。
pub(super) async fn abort_and_join<T>(handle: tokio::task::JoinHandle<T>) {
    handle.abort();
    let _ = handle.await;
}

/// 为子循环创建独立取消通道；父层取消时先通知子层，再等待子层自行收尾退出。
///
/// 用于 `execute_turn` 中 ctx 已被子循环 future 独占借用、父层 select 无法直接
/// await 子循环的场景：父层持有 `parent_cancel`，子循环拿到新分配的 `cancel_rx`。
/// 父层取消信号到来时，先向子循环发送取消，再 await 子循环返回其（部分）结果，
/// 保证 ctx 的可变借用被有序释放、迟到事件不会越过轮次屏障。
pub(super) async fn run_cancelable_child<T, F, Fut>(
    parent_cancel: &mut oneshot::Receiver<()>,
    child: F,
) -> T
where
    F: FnOnce(oneshot::Receiver<()>) -> Fut,
    Fut: Future<Output = T>,
{
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let mut child = Box::pin(child(cancel_rx));
    tokio::select! {
        biased;
        _ = &mut *parent_cancel => {
            let _ = cancel_tx.send(());
            child.await
        }
        result = &mut child => result,
    }
}
