/// 上下文阈值计算器
///
/// 仅负责压缩阈值的计算与判断。压缩任务生命周期由 `react::compression` 管理。
pub struct ContextOrganizer {
    /// 模型上下文限制（token 数）
    context_limit: usize,
    /// 用户要求的更早触发点；不能突破派生的安全上限。
    requested_threshold: Option<usize>,
}

impl ContextOrganizer {
    const COMPRESSION_OUTPUT_DIVISOR: usize = 20;
    const SAFETY_MARGIN_DIVISOR: usize = 100;
    const MIN_SAFETY_MARGIN_TOKENS: usize = 4_096;
    pub const MIN_COMPRESSION_OUTPUT_TOKENS: usize = 2_048;

    pub fn new(context_limit: usize) -> Self {
        Self {
            context_limit,
            requested_threshold: None,
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.requested_threshold =
            Some((self.context_limit as f64 * threshold.clamp(0.0, 1.0)) as usize);
        self
    }

    pub fn with_max_context_tokens(mut self, max_tokens: usize) -> Self {
        self.requested_threshold = Some(max_tokens.min(self.context_limit));
        self
    }

    /// 压缩输出目标：当前上下文窗口的 5%。
    pub fn target_output_tokens(&self) -> usize {
        self.context_limit / Self::COMPRESSION_OUTPUT_DIVISOR
    }

    /// 为压缩指令、Provider token 计数差异预留 1%，且至少保留 4096 tokens。
    pub fn safety_margin_tokens(&self) -> usize {
        (self.context_limit / Self::SAFETY_MARGIN_DIVISOR).max(Self::MIN_SAFETY_MARGIN_TOKENS)
    }

    fn safe_threshold(&self) -> usize {
        self.context_limit
            .saturating_sub(self.target_output_tokens())
            .saturating_sub(self.safety_margin_tokens())
    }

    /// 压缩阈值（token 数）
    pub fn token_threshold(&self) -> usize {
        self.requested_threshold.map_or_else(
            || self.safe_threshold(),
            |requested| requested.min(self.safe_threshold()),
        )
    }

    /// 基于 API 返回的输入与输出总量判断是否需要压缩。
    pub fn needs_compression(&self, observed_total_tokens: usize) -> bool {
        observed_total_tokens >= self.token_threshold()
    }

    /// 根据已观测总量收紧本次压缩的最大输出。
    ///
    /// `observed_total_tokens == 0` 用于手动压缩或强制压缩：没有可靠用量时只使用
    /// 5% 目标值，最终是否满足 Provider 上限仍由请求层校验。
    pub fn compression_output_budget(&self, observed_total_tokens: usize) -> Option<u32> {
        let target = self.target_output_tokens();
        let available = if observed_total_tokens == 0 {
            target
        } else {
            self.context_limit
                .saturating_sub(observed_total_tokens)
                .saturating_sub(self.safety_margin_tokens())
                .min(target)
        };
        if available < Self::MIN_COMPRESSION_OUTPUT_TOKENS {
            return None;
        }
        Some(available.min(u32::MAX as usize) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::ContextOrganizer;

    #[test]
    fn derives_budget_for_200k_context() {
        let organizer = ContextOrganizer::new(200_000);

        assert_eq!(organizer.target_output_tokens(), 10_000);
        assert_eq!(organizer.safety_margin_tokens(), 4_096);
        assert_eq!(organizer.token_threshold(), 185_904);
    }

    #[test]
    fn derives_budget_for_one_million_context() {
        let organizer = ContextOrganizer::new(1_000_000);

        assert_eq!(organizer.target_output_tokens(), 50_000);
        assert_eq!(organizer.safety_margin_tokens(), 10_000);
        assert_eq!(organizer.token_threshold(), 940_000);
        assert_eq!(organizer.compression_output_budget(950_000), Some(40_000));
    }

    #[test]
    fn scales_without_a_fixed_one_million_branch() {
        let organizer = ContextOrganizer::new(2_000_000);

        assert_eq!(organizer.target_output_tokens(), 100_000);
        assert_eq!(organizer.safety_margin_tokens(), 20_000);
        assert_eq!(organizer.token_threshold(), 1_880_000);
    }

    #[test]
    fn user_threshold_can_only_trigger_earlier() {
        let earlier = ContextOrganizer::new(200_000).with_max_context_tokens(120_000);
        let later = ContextOrganizer::new(200_000).with_threshold(0.99);

        assert_eq!(earlier.token_threshold(), 120_000);
        assert_eq!(later.token_threshold(), 185_904);
    }

    #[test]
    fn rejects_an_output_budget_below_the_minimum() {
        let organizer = ContextOrganizer::new(200_000);

        assert_eq!(organizer.compression_output_budget(194_000), None);
    }
}
