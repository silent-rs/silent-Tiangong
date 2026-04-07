//! 查询编排层类型定义

use serde::{Deserialize, Serialize};

/// 查询执行模式
///
/// 由查询编排层判断，决定当前输入走哪种执行路径。
/// 替代原 `context::assembler::QueryMode` 的二分法，提供更细粒度的路由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    /// 直接回答：不注入工具定义（简单对话、闲聊、知识问答）
    DirectAnswer,
    /// 单工具执行：简单的单步工具调用
    SingleToolExecution,
    /// 多步工具链执行：需要规划 + 多轮工具调用
    MultiStepExecution,
    /// 子任务拆分：拆分为多个独立子任务并行执行（多代理）
    TaskSplit,
    /// 后台执行：长时间运行的任务移至后台
    BackgroundExecution,
}

impl QueryMode {
    /// 是否需要注入工具定义
    pub fn needs_tools(&self) -> bool {
        !matches!(self, Self::DirectAnswer)
    }

    /// 是否需要规划阶段
    pub fn needs_planning(&self) -> bool {
        matches!(self, Self::MultiStepExecution | Self::TaskSplit)
    }

    /// 是否需要多代理协调
    pub fn needs_coordinator(&self) -> bool {
        matches!(self, Self::TaskSplit)
    }

    /// 是否应在后台执行
    pub fn is_background(&self) -> bool {
        matches!(self, Self::BackgroundExecution)
    }

    /// 获取简短显示名
    pub fn display_name(&self) -> &str {
        match self {
            Self::DirectAnswer => "直接回答",
            Self::SingleToolExecution => "单工具执行",
            Self::MultiStepExecution => "多步执行",
            Self::TaskSplit => "子任务拆分",
            Self::BackgroundExecution => "后台执行",
        }
    }
}

impl std::fmt::Display for QueryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// 编排决策结果
///
/// `QueryOrchestrator` 的输出，包含路由决策和附加信息。
#[derive(Debug, Clone)]
pub struct OrchestratorDecision {
    /// 选择的执行模式
    pub mode: QueryMode,
    /// 是否应打断当前正在运行的任务
    pub should_interrupt: bool,
    /// 决策理由（调试用）
    pub reason: String,
    /// 编排过程中的 LLM 调用记录
    pub llm_calls: Vec<crate::session::LlmCallRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_answer_no_tools() {
        assert!(!QueryMode::DirectAnswer.needs_tools());
        assert!(!QueryMode::DirectAnswer.needs_planning());
        assert!(!QueryMode::DirectAnswer.needs_coordinator());
    }

    #[test]
    fn single_tool_needs_tools_no_planning() {
        assert!(QueryMode::SingleToolExecution.needs_tools());
        assert!(!QueryMode::SingleToolExecution.needs_planning());
    }

    #[test]
    fn multi_step_needs_planning() {
        assert!(QueryMode::MultiStepExecution.needs_tools());
        assert!(QueryMode::MultiStepExecution.needs_planning());
        assert!(!QueryMode::MultiStepExecution.needs_coordinator());
    }

    #[test]
    fn task_split_needs_coordinator() {
        assert!(QueryMode::TaskSplit.needs_tools());
        assert!(QueryMode::TaskSplit.needs_planning());
        assert!(QueryMode::TaskSplit.needs_coordinator());
    }

    #[test]
    fn background_mode() {
        assert!(QueryMode::BackgroundExecution.is_background());
        assert!(!QueryMode::DirectAnswer.is_background());
    }

    #[test]
    fn serialization_roundtrip() {
        let mode = QueryMode::MultiStepExecution;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, r#""multi_step_execution""#);
        let parsed: QueryMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, mode);
    }

    #[test]
    fn display_names_non_empty() {
        let modes = [
            QueryMode::DirectAnswer,
            QueryMode::SingleToolExecution,
            QueryMode::MultiStepExecution,
            QueryMode::TaskSplit,
            QueryMode::BackgroundExecution,
        ];
        for m in &modes {
            assert!(!m.display_name().is_empty());
            assert!(!format!("{m}").is_empty());
        }
    }
}
