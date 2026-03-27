use crate::context::compressor::{CompressionStrategy, ContextCompressor};
use crate::model::SingleProviderClient;
use crate::session::{Message, Session};

/// token 估算：按字符数近似（中文约 2 token/字，英文约 0.75 token/word）
/// 这里使用简单的字符数 / 2 作为近似值
fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|msg| {
            let content_len = msg.content.len();
            let reasoning_len = msg.reasoning_content.len();
            (content_len + reasoning_len) / 2
        })
        .sum()
}

/// 上下文组织器
///
/// 从 Session 消息构建 LLM 请求上下文，
/// 自动判断是否需要压缩。
pub struct ContextOrganizer {
    /// 模型上下文限制（token 数）
    context_limit: usize,
    /// 触发压缩的阈值比例（默认 0.8）
    compression_threshold: f64,
    /// 压缩器
    compressor: ContextCompressor,
}

impl ContextOrganizer {
    pub fn new(context_limit: usize) -> Self {
        Self {
            context_limit,
            compression_threshold: 0.8,
            compressor: ContextCompressor::default(),
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.compression_threshold = threshold;
        self
    }

    pub fn with_compressor(mut self, compressor: ContextCompressor) -> Self {
        self.compressor = compressor;
        self
    }

    /// 判断当前消息是否需要压缩
    pub fn needs_compression(&self, messages: &[Message]) -> bool {
        let estimated_tokens = estimate_tokens(messages);
        let threshold = (self.context_limit as f64 * self.compression_threshold) as usize;
        estimated_tokens > threshold
    }

    /// 构建 LLM 请求上下文
    ///
    /// 如果消息总 token 超过阈值，自动执行压缩
    pub fn build_context(
        &self,
        session: &Session,
        client: Option<&SingleProviderClient>,
    ) -> anyhow::Result<Vec<Message>> {
        let messages = &session.messages;

        if !self.needs_compression(messages) {
            return Ok(messages.clone());
        }

        tracing::info!(
            estimated_tokens = estimate_tokens(messages),
            context_limit = self.context_limit,
            threshold = %self.compression_threshold,
            "上下文超过阈值，执行压缩"
        );

        self.compressor.compress(messages, client)
    }

    /// 获取推荐的压缩策略
    pub fn recommended_strategy(
        &self,
        messages: &[Message],
        has_client: bool,
    ) -> CompressionStrategy {
        if has_client && estimate_tokens(messages) > 1000 {
            CompressionStrategy::LlmSummary
        } else {
            CompressionStrategy::SlidingWindow
        }
    }
}
