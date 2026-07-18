/// 上下文阈值计算器
///
/// 仅负责压缩阈值的计算与判断。实际压缩由 `ContextCompressor` 承担
/// （它持有 `&mut TurnContext`，通过 `new(ctx)` + `compress()` 使用）。
pub struct ContextOrganizer {
    /// 模型上下文限制（token 数）
    context_limit: usize,
    /// 触发压缩的阈值比例（默认 0.95，接近模型限制前压缩）
    compression_threshold: f64,
}

impl ContextOrganizer {
    pub fn new(context_limit: usize) -> Self {
        Self {
            context_limit,
            compression_threshold: 0.95,
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

    /// 压缩阈值（token 数）
    pub fn token_threshold(&self) -> usize {
        (self.context_limit as f64 * self.compression_threshold) as usize
    }

    /// 基于 API 返回的精确 prompt_tokens 判断是否需要压缩
    pub fn needs_compression(&self, actual_prompt_tokens: usize) -> bool {
        actual_prompt_tokens > self.token_threshold()
    }
}
