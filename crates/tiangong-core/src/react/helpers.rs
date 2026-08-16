//! ReAct 主循环的通用辅助函数。
//!
//! 这里只保留不参与命令路由的独立逻辑：
//! - 插件模型用量累计与事件发布（`record_plugin_usage`）
//!
//! 浏览器页面自动观察已随 PageFetcher 能力下沉迁入 browser 插件（#225），
//! core 不再感知浏览器快照注入。

use crate::model::TokenUsage;
use tiangong_types::StreamEvent;

pub(super) fn record_plugin_usage(
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    context_limit: usize,
    accumulated_usage: &mut TokenUsage,
    usage: TokenUsage,
    source: String,
    emit_event: bool,
) {
    accumulated_usage.accumulate(&usage);
    if emit_event {
        crate::react::context::emit_token_usage(
            stream_tx,
            &usage,
            None,
            context_limit,
            source,
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_usage_is_accumulated_and_optionally_emitted() {
        let (stream_tx, stream_rx) = std::sync::mpsc::channel();
        let usage = TokenUsage {
            prompt_tokens: 7,
            completion_tokens: 5,
            total_tokens: 12,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };
        let mut accumulated = TokenUsage::default();

        record_plugin_usage(
            &stream_tx,
            1_000,
            &mut accumulated,
            usage,
            "plugin".to_string(),
            true,
        );

        assert_eq!(accumulated.total_tokens, 12);
        let StreamEvent::TokenUsage { source, .. } = stream_rx.try_recv().unwrap() else {
            panic!("expected token usage event");
        };
        assert_eq!(source, "plugin");
    }
}
