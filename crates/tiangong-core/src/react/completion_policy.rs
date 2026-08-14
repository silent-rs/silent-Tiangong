//! 完成度策略（任务 10，ALR-303）：Summary/ForceFinal 的触发判定从执行驱动中
//! 解耦为可替换策略。
//!
//! 默认策略 `DefaultCompletionPolicy` 完整保持既有行为（任务 10 只做架构解耦，
//! 不改变默认触发）。实验策略 `OnDemandCompletionPolicy`（按需完成度检查：明显
//! 完整的答复直接完成）**默认不启用**——须先积累对照数据（质量/延迟/请求数/
//! token），指标达标并确认回滚路径后才允许切换（design.md 8）。

/// 文本回复完成后的去向。
pub(super) enum CompletionAction {
    /// 直接作为最终答复（跳过独立完成度检查请求）。
    Finish,
    /// 进入完成度检查（现行 Summary 请求）。
    CheckCompletion,
}

/// 完成度判定的输入快照（不含敏感正文，仅判定所需的结构化信号）。
pub(super) struct TextCompletionInput {
    /// 完成度检查要求继续的次数（0 = 尚未进入过检查）。
    pub(super) continuation_count: u32,
    /// 当前阶段是否执行过工具。
    pub(super) executed_tool_in_phase: bool,
    /// 回复文本是否为空。
    pub(super) text_empty: bool,
    /// 回复文本是否"看起来像最终答复"（启发式）。
    pub(super) looks_like_final: bool,
    /// 是否为合成工具调用占位符（协议修复产物，必须进检查）。
    pub(super) synthetic_placeholder: bool,
}

/// 完成度策略：决定 ReAct 文本回复完成后是否需要独立的完成度检查。
pub(super) trait CompletionPolicy: Send + Sync {
    fn name(&self) -> &'static str;
    fn decide_after_text(&self, input: &TextCompletionInput) -> CompletionAction;
}

/// 现行默认策略：保持既有行为。
///
/// 1. 合成占位符 → 检查（协议修复产物不能作为最终答复）；
/// 2. 未进入过检查且本阶段未执行工具的非空回复 → 直接完成（首轮直接回答）；
/// 3. 执行过工具且文本"看起来像最终答复" → 直接完成（工具后完整答复）；
/// 4. 其余 → 检查。
pub(super) struct DefaultCompletionPolicy;

impl CompletionPolicy for DefaultCompletionPolicy {
    fn name(&self) -> &'static str {
        "default"
    }

    fn decide_after_text(&self, input: &TextCompletionInput) -> CompletionAction {
        let action = default_decide(input);
        // 触发指标日志（任务 10 项 2）：记录判定输入与结果，供后续对照分析。
        tracing::debug!(
            policy = self.name(),
            continuation_count = input.continuation_count,
            executed_tool = input.executed_tool_in_phase,
            text_empty = input.text_empty,
            looks_like_final = input.looks_like_final,
            action = matches!(action, CompletionAction::Finish),
            "完成度判定"
        );
        action
    }
}

fn default_decide(input: &TextCompletionInput) -> CompletionAction {
    if input.synthetic_placeholder {
        return CompletionAction::CheckCompletion;
    }
    let direct_answer =
        input.continuation_count == 0 && !input.executed_tool_in_phase && !input.text_empty;
    let tool_answer = input.executed_tool_in_phase && input.looks_like_final;
    if direct_answer || tool_answer {
        CompletionAction::Finish
    } else {
        CompletionAction::CheckCompletion
    }
}

/// 实验策略（按需完成度检查，**默认不启用**）：在默认规则之上，把"执行过工具
/// 且文本非空"的答复也直接完成，仅对空回复/占位符/续作后不明输出保留检查。
///
/// 启用前提（design.md 8）：同一代表性场景的对照数据显示任务完成率不明显下降、
/// 且延迟/请求数/token 收益达到约定阈值；须保留回滚开关。启用前由测试构造验证，
/// 生产未接入（dead_code 豁免）。
#[allow(dead_code)]
pub(super) struct OnDemandCompletionPolicy;

impl CompletionPolicy for OnDemandCompletionPolicy {
    fn name(&self) -> &'static str {
        "on_demand"
    }

    fn decide_after_text(&self, input: &TextCompletionInput) -> CompletionAction {
        if input.synthetic_placeholder || input.text_empty {
            return CompletionAction::CheckCompletion;
        }
        if input.continuation_count > 0 && !input.looks_like_final {
            // 续作后仍不完整的输出保留检查（保守面）。
            return CompletionAction::CheckCompletion;
        }
        CompletionAction::Finish
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        continuation_count: u32,
        executed_tool: bool,
        text_empty: bool,
        looks_like_final: bool,
        synthetic: bool,
    ) -> TextCompletionInput {
        TextCompletionInput {
            continuation_count,
            executed_tool_in_phase: executed_tool,
            text_empty,
            looks_like_final,
            synthetic_placeholder: synthetic,
        }
    }

    #[test]
    fn default_policy_preserves_current_behavior() {
        let policy = DefaultCompletionPolicy;
        // 首轮直接回答 → 完成。
        assert!(matches!(
            policy.decide_after_text(&input(0, false, false, false, false)),
            CompletionAction::Finish
        ));
        // 首轮空回复 → 检查。
        assert!(matches!(
            policy.decide_after_text(&input(0, false, true, false, false)),
            CompletionAction::CheckCompletion
        ));
        // 工具后"看起来像最终答复" → 完成。
        assert!(matches!(
            policy.decide_after_text(&input(0, true, false, true, false)),
            CompletionAction::Finish
        ));
        // 工具后不像最终答复（如问号结尾）→ 检查。
        assert!(matches!(
            policy.decide_after_text(&input(0, true, false, false, false)),
            CompletionAction::CheckCompletion
        ));
        // 续作过的直接回答 → 检查（continuation_count > 0）。
        assert!(matches!(
            policy.decide_after_text(&input(1, false, false, true, false)),
            CompletionAction::CheckCompletion
        ));
        // 合成占位符 → 始终检查。
        assert!(matches!(
            policy.decide_after_text(&input(0, false, false, true, true)),
            CompletionAction::CheckCompletion
        ));
    }

    #[test]
    fn on_demand_policy_finishes_more_but_keeps_conservative_cases() {
        let policy = OnDemandCompletionPolicy;
        // 工具后非空（即使不像最终答复）→ 完成（实验放宽面）。
        assert!(matches!(
            policy.decide_after_text(&input(0, true, false, false, false)),
            CompletionAction::Finish
        ));
        // 保守面：空回复/占位符/续作后不完整 → 检查。
        assert!(matches!(
            policy.decide_after_text(&input(0, true, true, false, false)),
            CompletionAction::CheckCompletion
        ));
        assert!(matches!(
            policy.decide_after_text(&input(0, false, false, false, true)),
            CompletionAction::CheckCompletion
        ));
        assert!(matches!(
            policy.decide_after_text(&input(1, true, false, false, false)),
            CompletionAction::CheckCompletion
        ));
    }
}
