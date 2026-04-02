//! Token 预算控制器
//!
//! 管理单次 LLM 调用的 token 预算分配，确保上下文不超出模型限制。

use crate::model::FunctionToolSpec;

use super::organizer::estimate_tokens;

/// Token 预算分配
#[derive(Debug, Clone)]
pub struct TokenBudget {
    /// 模型上下文窗口总 token 数
    pub context_limit: usize,
    /// 预留给模型输出的 token 数（默认上下文的 30%）
    pub output_reserve_ratio: f64,
}

impl TokenBudget {
    pub fn new(context_limit: usize) -> Self {
        Self {
            context_limit,
            output_reserve_ratio: 0.3,
        }
    }

    /// 可用于输入（prompt）的最大 token 数
    pub fn max_prompt_tokens(&self) -> usize {
        (self.context_limit as f64 * (1.0 - self.output_reserve_ratio)) as usize
    }

    /// 估算工具定义占用的 token 数
    ///
    /// 每个工具定义约 150-300 tokens（名称、描述、参数 schema）
    pub fn estimate_tools_tokens(tools: &[FunctionToolSpec]) -> usize {
        tools.iter().map(|t| {
            let name_len = t.name.len();
            let desc_len = t.description.len();
            let params_len = t.parameters.to_string().len();
            // 粗略估算：字符数 * 0.4（JSON schema 多为英文/符号）
            ((name_len + desc_len + params_len) as f64 * 0.4) as usize + 20
        }).sum()
    }

    /// 计算在给定工具和历史消息下的剩余可用 token
    pub fn remaining_for_input(
        &self,
        tools: &[FunctionToolSpec],
        messages: &[crate::session::Message],
    ) -> usize {
        let max = self.max_prompt_tokens();
        let tools_cost = Self::estimate_tools_tokens(tools);
        let messages_cost = estimate_tokens(messages);
        max.saturating_sub(tools_cost + messages_cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_prompt_tokens_respects_reserve() {
        let budget = TokenBudget::new(10000);
        assert_eq!(budget.max_prompt_tokens(), 7000); // 70% of 10000
    }

    #[test]
    fn empty_tools_zero_cost() {
        assert_eq!(TokenBudget::estimate_tools_tokens(&[]), 0);
    }

    #[test]
    fn tool_cost_scales_with_content() {
        let small = FunctionToolSpec {
            name: "read".into(),
            description: "Read file".into(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let large = FunctionToolSpec {
            name: "run_command".into(),
            description: "Execute a shell command with arguments and environment variables".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "cmd": {"type": "string", "description": "Command to execute"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "cwd": {"type": "string", "description": "Working directory"}
                },
                "required": ["cmd"]
            }),
        };
        let small_cost = TokenBudget::estimate_tools_tokens(&[small]);
        let large_cost = TokenBudget::estimate_tools_tokens(&[large]);
        assert!(large_cost > small_cost);
    }
}
