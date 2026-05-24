use crate::context::compressor::{CompressionUpdate, ContextCompressor};
use crate::model::SingleProviderClient;
use crate::session::{Message, Session};

/// 上下文组织器
///
/// 管理对话上下文的构建与压缩策略。
/// 采用滚动摘要机制：摘要持久化到 Session，原始消息保持完整。
pub struct ContextOrganizer {
    /// 模型上下文限制（token 数）
    context_limit: usize,
    /// 触发压缩的阈值比例（默认 0.95，接近模型限制前压缩）
    compression_threshold: f64,
    /// 压缩器
    compressor: ContextCompressor,
}

impl ContextOrganizer {
    pub fn new(context_limit: usize) -> Self {
        Self {
            context_limit,
            compression_threshold: 0.95,
            compressor: ContextCompressor::default(),
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.compression_threshold = threshold;
        self
    }

    pub fn with_max_context_tokens(mut self, max_tokens: usize) -> Self {
        if self.context_limit > 0 {
            self.compression_threshold =
                (max_tokens as f64 / self.context_limit as f64).clamp(0.0, 1.0);
        }
        self
    }

    pub fn with_keep_recent_turns(mut self, turns: usize) -> Self {
        self.compressor = ContextCompressor::new(turns);
        self
    }

    /// 压缩阈值（token 数）
    pub fn token_threshold(&self) -> usize {
        (self.context_limit as f64 * self.compression_threshold) as usize
    }

    /// 基于 API 返回的精确 prompt_tokens 判断是否需要压缩
    pub fn needs_compression(&self, actual_prompt_tokens: usize) -> bool {
        actual_prompt_tokens > self.token_threshold()
    }

    /// 基于 API 返回的精确 prompt_tokens 更新会话摘要（如果需要）
    ///
    /// 检查实际 prompt token 是否超过阈值，如果超过则更新 session 的滚动摘要。
    /// 摘要持久化到 session，后续 turn 不会重复压缩。
    pub fn maybe_update_summary(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
        actual_prompt_tokens: usize,
    ) -> anyhow::Result<bool> {
        Ok(self
            .maybe_update_summary_with_usage(session, client, actual_prompt_tokens)?
            .compressed)
    }

    pub fn maybe_update_summary_with_usage(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
        actual_prompt_tokens: usize,
    ) -> anyhow::Result<CompressionUpdate> {
        if actual_prompt_tokens == 0 || !self.needs_compression(actual_prompt_tokens) {
            return Ok(CompressionUpdate::default());
        }
        self.compressor.update_summary_with_usage(session, client)
    }

    /// 强制压缩上下文（忽略 token 阈值检查）
    pub fn force_update_summary(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
    ) -> anyhow::Result<bool> {
        Ok(self
            .force_update_summary_with_usage(session, client)?
            .compressed)
    }

    pub fn force_update_summary_with_usage(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
    ) -> anyhow::Result<CompressionUpdate> {
        self.compressor.update_summary_with_usage(session, client)
    }

    /// 构建 LLM 请求上下文
    ///
    /// 直接返回 session.messages 中尚未被摘要覆盖的消息，不做过滤。
    pub fn build_context(&self, session: &Session) -> Vec<Message> {
        self.compressor.build_context(session)
    }
}
