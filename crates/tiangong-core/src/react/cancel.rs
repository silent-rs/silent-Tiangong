//! 活跃模型任务的取消与增量用量上报。

use std::sync::mpsc::Sender as StdSender;

use tiangong_types::StreamEvent;

use crate::react::context::emit_token_usage;

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
