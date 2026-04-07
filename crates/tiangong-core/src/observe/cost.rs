//! Token 成本追踪
//!
//! 按请求级、任务级、会话级聚合 Token 使用量和成本。

use serde::{Deserialize, Serialize};

use crate::model::TokenUsage;

/// 成本摘要
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostSummary {
    /// 总 prompt tokens
    pub total_prompt_tokens: usize,
    /// 总 completion tokens
    pub total_completion_tokens: usize,
    /// 总 tokens
    pub total_tokens: usize,
    /// LLM 调用次数
    pub call_count: usize,
    /// 工具调用次数
    pub tool_call_count: usize,
}

impl CostSummary {
    /// 累加一次 LLM 调用的 usage
    pub fn add_llm_usage(&mut self, usage: &TokenUsage) {
        self.total_prompt_tokens += usage.prompt_tokens;
        self.total_completion_tokens += usage.completion_tokens;
        self.total_tokens += usage.total_tokens;
        self.call_count += 1;
    }

    /// 累加工具调用
    pub fn add_tool_call(&mut self) {
        self.tool_call_count += 1;
    }

    /// 合并另一个摘要
    pub fn merge(&mut self, other: &CostSummary) {
        self.total_prompt_tokens += other.total_prompt_tokens;
        self.total_completion_tokens += other.total_completion_tokens;
        self.total_tokens += other.total_tokens;
        self.call_count += other.call_count;
        self.tool_call_count += other.tool_call_count;
    }
}

/// 请求级成本（单次 LLM 调用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestCost {
    /// 请求标识
    pub request_id: String,
    /// Token 使用量
    pub usage: TokenUsage,
    /// 请求时间
    pub timestamp: String,
}

/// 任务级成本（聚合多个 RequestCost）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskCost {
    /// 任务 ID
    pub task_id: String,
    /// 该任务中所有请求的成本
    pub requests: Vec<RequestCost>,
    /// 聚合摘要
    pub summary: CostSummary,
}

impl TaskCost {
    pub fn new(task_id: String) -> Self {
        Self {
            task_id,
            requests: Vec::new(),
            summary: CostSummary::default(),
        }
    }

    /// 记录一次请求成本
    pub fn add_request(&mut self, request_id: String, usage: &TokenUsage) {
        self.requests.push(RequestCost {
            request_id,
            usage: usage.clone(),
            timestamp: chrono::Local::now().naive_local().to_string(),
        });
        self.summary.add_llm_usage(usage);
    }
}

/// 会话级成本（聚合多个 TaskCost）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionCost {
    /// 会话 ID
    pub session_id: String,
    /// 该会话中所有任务的成本
    pub tasks: Vec<TaskCost>,
    /// 聚合摘要
    pub summary: CostSummary,
}

impl SessionCost {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            tasks: Vec::new(),
            summary: CostSummary::default(),
        }
    }

    /// 添加一个任务的成本
    pub fn add_task(&mut self, task_cost: TaskCost) {
        self.summary.merge(&task_cost.summary);
        self.tasks.push(task_cost);
    }
}

/// 从 Session 的 task_records 计算会话级成本
pub fn calculate_session_cost(task_records: &[crate::session::SessionTaskRecord]) -> CostSummary {
    let mut summary = CostSummary::default();
    for record in task_records {
        if let Some(ref usage) = record.usage {
            summary.add_llm_usage(usage);
        }
        if record.tool_result.is_some() {
            summary.add_tool_call();
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_summary_add() {
        let mut summary = CostSummary::default();
        summary.add_llm_usage(&TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        });
        summary.add_llm_usage(&TokenUsage {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
        });
        summary.add_tool_call();

        assert_eq!(summary.total_prompt_tokens, 300);
        assert_eq!(summary.total_completion_tokens, 150);
        assert_eq!(summary.total_tokens, 450);
        assert_eq!(summary.call_count, 2);
        assert_eq!(summary.tool_call_count, 1);
    }

    #[test]
    fn cost_summary_merge() {
        let mut a = CostSummary {
            total_tokens: 100,
            call_count: 2,
            ..Default::default()
        };
        let b = CostSummary {
            total_tokens: 200,
            call_count: 3,
            tool_call_count: 1,
            ..Default::default()
        };
        a.merge(&b);
        assert_eq!(a.total_tokens, 300);
        assert_eq!(a.call_count, 5);
        assert_eq!(a.tool_call_count, 1);
    }
}
