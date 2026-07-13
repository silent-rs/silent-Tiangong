//! 取消原语：ReAct 主循环与总结阶段共享的取消处理基础设施。
//!
//! `select!` 循环 break 时使用可被 anyhow downcast 的取消信号类型，替代裸字符串。

use std::sync::mpsc::Sender as StdSender;

use tiangong_types::StreamEvent;

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

/// 取消时统一上报 token usage 并发送 Error 事件。
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
    emit_cancelled(stream_tx);
}

/// 取消时仅发送终态，不重复上报已经发出过的用量。
pub(super) fn emit_cancelled(stream_tx: &StdSender<StreamEvent>) {
    let _ = stream_tx.send(StreamEvent::Error {
        message: "已取消".into(),
    });
}

/// 请求取消异步 LLM 任务并等待其真正退出，确保它不再越过轮次屏障发送迟到事件。
pub(super) async fn abort_and_join<T>(handle: tokio::task::JoinHandle<T>) {
    handle.abort();
    let _ = handle.await;
}
