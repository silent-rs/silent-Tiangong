use crate::context::compressor::ContextCompressor;
use crate::model::SingleProviderClient;
use crate::session::{Message, Session};

/// token 估算（仅用于首次调用前的预判，后续应使用 API 返回的精确值）
///
/// 中文约 1.5 token/字符，英文约 0.75 token/word，
/// 使用 字符数 * 0.6 作为混合场景近似值。
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|msg| {
            let content_chars = msg.content.chars().count();
            let reasoning_chars = msg.reasoning_content.chars().count();
            // 每条消息额外 4 token 开销（role/separator）
            ((content_chars + reasoning_chars) as f64 * 0.6) as usize + 4
        })
        .sum()
}

/// 上下文组织器
///
/// 从 Session 消息构建 LLM 请求上下文，
/// 自动判断是否需要压缩，替代硬编码的滑动窗口。
pub struct ContextOrganizer {
    /// 模型上下文限制（token 数）
    context_limit: usize,
    /// 触发压缩的阈值比例（默认 0.7，留 30% 给当前请求和回复）
    compression_threshold: f64,
    /// 压缩器
    compressor: ContextCompressor,
}

impl ContextOrganizer {
    pub fn new(context_limit: usize) -> Self {
        Self {
            context_limit,
            compression_threshold: 0.7,
            compressor: ContextCompressor::default(),
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.compression_threshold = threshold;
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

    /// 基于估算判断是否需要压缩（首次调用前使用）
    pub fn needs_compression_estimated(&self, messages: &[Message]) -> bool {
        estimate_tokens(messages) > self.token_threshold()
    }

    /// 基于 API 返回的精确 prompt_tokens 判断是否需要压缩
    pub fn needs_compression(&self, actual_prompt_tokens: usize) -> bool {
        actual_prompt_tokens > self.token_threshold()
    }

    /// 构建 LLM 请求上下文
    ///
    /// 过滤执行痕迹，如果估算 token 超过阈值则自动压缩。
    /// 首次调用使用估算值；后续轮次应使用 `compress_if_needed` 基于精确值触发。
    pub fn build_context(
        &self,
        session: &Session,
        client: Option<&SingleProviderClient>,
    ) -> anyhow::Result<Vec<Message>> {
        let filtered = Self::filter_execution_traces(&session.messages);

        if !self.needs_compression_estimated(&filtered) {
            return Ok(filtered);
        }

        tracing::info!(
            estimated_tokens = estimate_tokens(&filtered),
            context_limit = self.context_limit,
            threshold = %self.compression_threshold,
            "上下文超过阈值（估算），执行压缩"
        );

        self.compressor.compress(&filtered, client)
    }

    /// 基于 API 返回的精确 prompt_tokens 对消息进行压缩
    ///
    /// 当 `actual_prompt_tokens` 超过阈值时执行压缩，返回 Some(压缩后消息)；
    /// 未超过阈值返回 None。
    pub fn compress_if_needed(
        &self,
        messages: &[Message],
        actual_prompt_tokens: usize,
        client: Option<&SingleProviderClient>,
    ) -> anyhow::Result<Option<Vec<Message>>> {
        if !self.needs_compression(actual_prompt_tokens) {
            return Ok(None);
        }

        tracing::info!(
            actual_prompt_tokens,
            threshold = self.token_threshold(),
            "上下文超过阈值（精确），执行压缩"
        );

        self.compressor.compress(messages, client).map(Some)
    }

    /// 过滤执行阶段的 System 消息，保留纯对话
    pub fn filter_execution_traces(messages: &[Message]) -> Vec<Message> {
        messages
            .iter()
            .filter(|msg| {
                if msg.role != crate::session::MessageRole::System {
                    return true;
                }
                let c = msg.content.as_str();
                !(c.starts_with("工具执行")
                    || c.starts_with("LLM 输出")
                    || c.starts_with("Plan 执行总结")
                    || c.starts_with("检测到")
                    || c.starts_with("执行已取消"))
            })
            .cloned()
            .collect()
    }
}
