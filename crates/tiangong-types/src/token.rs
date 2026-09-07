//! Token 用量类型

use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};

/// Token 使用量
#[derive(Debug, Clone, Default, Deserialize)]
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
    /// 仅完整的输入缓存统计可以计算命中率；缺失部分不得视为零命中。
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let hit = self.prompt_cache_hit_tokens?;
        let miss = self.prompt_cache_miss_tokens?;
        if self.prompt_tokens == 0 || hit.checked_add(miss)? != self.prompt_tokens {
            return None;
        }
        Some(hit as f64 / self.prompt_tokens as f64)
    }

    pub fn has_usage(&self) -> bool {
        self.prompt_tokens > 0
            || self.completion_tokens > 0
            || self.total_tokens > 0
            || self.prompt_cache_hit_tokens.is_some()
            || self.prompt_cache_miss_tokens.is_some()
    }

    /// 同一次流式请求的累计快照，只合并最新计数，不重复累加。
    pub fn merge_snapshot(&mut self, next: &Self) {
        self.prompt_tokens = self.prompt_tokens.max(next.prompt_tokens);
        self.completion_tokens = self.completion_tokens.max(next.completion_tokens);
        self.total_tokens = self
            .total_tokens
            .max(next.total_tokens)
            .max(self.prompt_tokens + self.completion_tokens);
        if next.prompt_cache_hit_tokens.is_some() {
            self.prompt_cache_hit_tokens = next.prompt_cache_hit_tokens;
        }
        if next.prompt_cache_miss_tokens.is_some() {
            self.prompt_cache_miss_tokens = next.prompt_cache_miss_tokens;
        }
    }
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

impl Serialize for TokenUsage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serializer.serialize_struct("TokenUsage", 6)?;
        value.serialize_field("prompt_tokens", &self.prompt_tokens)?;
        value.serialize_field("completion_tokens", &self.completion_tokens)?;
        value.serialize_field("total_tokens", &self.total_tokens)?;
        value.serialize_field("prompt_cache_hit_tokens", &self.prompt_cache_hit_tokens)?;
        value.serialize_field("prompt_cache_miss_tokens", &self.prompt_cache_miss_tokens)?;
        value.serialize_field("cache_hit_rate", &self.cache_hit_rate())?;
        value.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageUsage {
    #[serde(flatten)]
    pub tokens: TokenUsage,
    pub model: String,
    /// 会话执行单元 ID；独立 Agent 会话各自保留归属。
    pub agent_id: String,
    pub turn_id: Option<String>,
    pub source: String,
    pub status: crate::message::TurnStatus,
}

/// 两个可选 usize 相加：双方都有则相加，仅一方有则取该值，都无则 None。
fn sum_optional(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: usize, hit: Option<usize>) -> TokenUsage {
        TokenUsage {
            prompt_tokens: input,
            completion_tokens: 10,
            total_tokens: input + 10,
            prompt_cache_hit_tokens: hit,
            prompt_cache_miss_tokens: hit.map(|hit| input - hit),
        }
    }

    #[test]
    fn cache_rate_is_weighted_and_unknown_for_partial_coverage() {
        let mut total = usage(100, Some(100));
        total.accumulate(&usage(900, Some(0)));
        assert_eq!(total.cache_hit_rate(), Some(0.1));
        assert_eq!(serde_json::to_value(&total).unwrap()["cache_hit_rate"], 0.1);
        total.accumulate(&usage(100, None));
        assert_eq!(total.cache_hit_rate(), None);
        assert_eq!(usage(100, Some(0)).cache_hit_rate(), Some(0.0));
        assert_eq!(usage(0, Some(0)).cache_hit_rate(), None);
        assert_eq!(usage(100, None).cache_hit_rate(), None);
    }

    #[test]
    fn serialization_recomputes_rate_and_old_data_still_loads() {
        let mut value = serde_json::to_value(usage(100, Some(75))).unwrap();
        value["cache_hit_rate"] = serde_json::json!(0.99);
        let loaded: TokenUsage = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.cache_hit_rate(), Some(0.75));
        let old: TokenUsage = serde_json::from_value(
            serde_json::json!({"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}),
        )
        .unwrap();
        assert_eq!(old.cache_hit_rate(), None);
    }

    #[test]
    fn repeated_stream_snapshots_do_not_add_the_same_usage_twice() {
        let mut total = TokenUsage::default();
        total.merge_snapshot(&usage(100, Some(80)));
        total.merge_snapshot(&usage(100, Some(80)));
        total.merge_snapshot(&usage(120, Some(90)));
        assert_eq!(total.prompt_tokens, 120);
        assert_eq!(total.completion_tokens, 10);
        assert_eq!(total.prompt_cache_hit_tokens, Some(90));
        assert_eq!(total.cache_hit_rate(), Some(0.75));
    }
}
