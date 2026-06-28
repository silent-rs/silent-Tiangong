//! 取消原语：ReAct 主循环与总结阶段共享的取消处理基础设施。
//!
//! LLM 流式请求在被用户取消时，不同 Provider 协议对 usage 的返回时机不同，
//! 因此需要按协议区分处理策略；同时 `select!` 循环 break 时需要携带一个可
//! 被 anyhow downcast 的取消信号类型，替代裸字符串。本模块集中这两类原语。

use std::sync::mpsc::Sender as StdSender;

use tiangong_types::StreamEvent;

use crate::react::context::emit_token_usage;

/// 取消时 LLM 请求的处理策略，按 Provider 协议区分。
pub(super) enum CancelStrategy {
    /// Anthropic: usage 在 message_start 就返回 prompt_tokens，可直接 abort
    AbortWithStreamingUsage,
    /// OpenAI 兼容: usage 仅在流式最后一个 chunk 返回，需等请求完成
    WaitForUsage,
}

impl CancelStrategy {
    pub(super) fn from_protocol(protocol: tiangong_llm::model::ProviderProtocol) -> Self {
        match protocol {
            tiangong_llm::model::ProviderProtocol::Anthropic => {
                CancelStrategy::AbortWithStreamingUsage
            }
            tiangong_llm::model::ProviderProtocol::OpenAiChatCompletions
            | tiangong_llm::model::ProviderProtocol::DeepSeek => CancelStrategy::WaitForUsage,
        }
    }
}

/// select! 循环 break 时携带的取消信号，替代魔术字符串。
#[derive(Clone, Copy)]
pub(super) enum CancelSignal {
    /// 直接 abort LLM future，上报已累积的 streaming_usage
    Abort,
    /// 在后台等待 LLM 自然完成以获取 usage，前端立即响应取消
    WaitForUsage,
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
            CancelSignal::WaitForUsage => write!(f, "cancelled (wait for usage)"),
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
pub(super) fn emit_cancel_usage(
    stream_tx: &StdSender<StreamEvent>,
    usage: &tiangong_types::TokenUsage,
    context_limit: usize,
) {
    if usage.total_tokens > 0 {
        emit_token_usage(stream_tx, usage, None, context_limit, "cancelled", None);
    }
    let _ = stream_tx.send(StreamEvent::Error {
        message: "已取消".into(),
    });
}
