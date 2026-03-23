use crate::model::TokenUsage;
use crate::planner::PlanStepStatus;

#[derive(Debug, Clone)]
pub struct ExecutionLlmOutput {
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<String>,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone)]
pub struct ExecutionStepReport {
    pub step_name: String,
    pub status: PlanStepStatus,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct ExecutionStepResult {
    pub llm_output: Option<ExecutionLlmOutput>,
    pub report: ExecutionStepReport,
    pub next_step: Option<DynamicPlanStep>,
}

#[derive(Debug, Clone)]
pub struct DynamicPlanStep {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct LlmOutputRecord {
    pub stage: String,
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<String>,
    pub usage: TokenUsage,
}
