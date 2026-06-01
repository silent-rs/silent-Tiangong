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
    /// 累加另一个 TokenUsage
    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        if let (Some(a), Some(b)) = (
            self.prompt_cache_hit_tokens.as_mut(),
            other.prompt_cache_hit_tokens,
        ) {
            *a += b;
        }
        if let (Some(a), Some(b)) = (
            self.prompt_cache_miss_tokens.as_mut(),
            other.prompt_cache_miss_tokens,
        ) {
            *a += b;
        }
    }
}
