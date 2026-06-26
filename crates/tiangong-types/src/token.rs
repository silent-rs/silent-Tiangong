//! Token 用量类型

use serde::{Deserialize, Serialize};

/// Token 使用量
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    #[serde(default)]
    pub prompt_cache_hit_tokens: Option<usize>,
    #[serde(default)]
    pub prompt_cache_miss_tokens: Option<usize>,
}

impl TokenUsage {
    /// 累加另一个 TokenUsage。
    ///
    /// cache 字段累加时，若自身为 None 而对方为 Some，则取对方值，
    /// 避免跨阶段累加丢失 KV cache 命中统计。
    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        self.prompt_cache_hit_tokens =
            sum_optional(self.prompt_cache_hit_tokens, other.prompt_cache_hit_tokens);
        self.prompt_cache_miss_tokens = sum_optional(
            self.prompt_cache_miss_tokens,
            other.prompt_cache_miss_tokens,
        );
    }
}

/// 两个可选 usize 相加：双方都有则相加，仅一方有则取该值，都无则 None。
fn sum_optional(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}
