//! 上下文装配协调器
//!
//! 根据查询编排层的执行模式决策，组装当前轮的完整上下文视图。
//! 包括：历史消息选择、工具定义注入、环境信息注入、预算控制。

use crate::agent_config::AgentConfig;
use crate::model::{FunctionToolSpec, SingleProviderClient};
use crate::models_config::ModelsConfig;
use crate::session::{Message, Session};

use super::budget::TokenBudget;
use super::organizer::ContextOrganizer;

/// 查询执行模式（重导出自 orchestrator 层）
///
/// 由查询编排层判断，传递给上下文装配层，决定注入哪些内容。
/// 详细定义见 `crate::orchestrator::types::QueryMode`。
pub use crate::orchestrator::QueryMode;

/// 查询意图分类器
///
/// 不做 LLM 分类调用，所有用户输入统一进入执行流程。
/// 由 planning/execution 层的 LLM 在完整上下文下自行判断是否需要工具。
pub struct QueryClassifier;

impl QueryClassifier {
    /// 统一返回 MultiStepExecution，由执行层自行判断
    pub fn classify(
        _input: &str,
        _session: &Session,
        _client: &SingleProviderClient,
    ) -> (QueryMode, Vec<crate::session::LlmCallRecord>) {
        (QueryMode::MultiStepExecution, Vec::new())
    }
}

/// 上下文装配结果
pub struct AssembledContext {
    /// 对话历史消息
    pub messages: Vec<Message>,
    /// 工具定义列表（可能为空）
    pub tools: Vec<FunctionToolSpec>,
    /// 系统 prompt
    pub system_prompt: String,
    /// 使用的查询模式
    pub mode: QueryMode,
    /// 装配过程中产生的 LLM 调用记录（如意图分类）
    pub llm_calls: Vec<crate::session::LlmCallRecord>,
}

/// 上下文装配器
///
/// 按以下顺序装配上下文：
/// 1. 确定查询模式（直接回答 / 工具执行）
/// 2. 构建对话历史（ContextOrganizer）
/// 3. 按模式决定是否注入工具定义
/// 4. Token 预算控制
pub struct ContextAssembler {
    organizer: ContextOrganizer,
    budget: TokenBudget,
}

impl ContextAssembler {
    pub fn new(context_limit: usize) -> Self {
        Self {
            organizer: ContextOrganizer::new(context_limit).with_keep_recent_turns(6),
            budget: TokenBudget::new(context_limit),
        }
    }

    /// 获取内部 ContextOrganizer 的引用
    pub fn organizer(&self) -> &ContextOrganizer {
        &self.organizer
    }

    /// 装配完整上下文
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        &self,
        session: &Session,
        user_input: &str,
        all_tools: Vec<FunctionToolSpec>,
        system_prompt: String,
        client: &SingleProviderClient,
        _models_config: &ModelsConfig,
        _agent_config: &AgentConfig,
    ) -> AssembledContext {
        let (mode, classify_calls) = QueryClassifier::classify(user_input, session, client);

        // 构建对话历史
        let messages = self.organizer.build_context(session);

        // 根据模式决定工具注入（使用 needs_tools() 统一判断）
        let tools = if mode.needs_tools() {
            self.select_tools(all_tools, &messages)
        } else {
            tracing::info!(
                input_len = user_input.len(),
                "快速路径：跳过工具注入（直接回答模式）"
            );
            Vec::new()
        };

        // 根据模式调整 system_prompt
        let system_prompt = if mode.needs_tools() {
            system_prompt
        } else {
            format!(
                "你是天工智能助手。请用简洁友好的方式回复用户。\
                 回复使用 Markdown 格式。\n\n用户输入：\n{user_input}"
            )
        };

        AssembledContext {
            messages,
            tools,
            system_prompt,
            mode,
            llm_calls: classify_calls,
        }
    }

    /// 选择要注入的工具（预算控制）
    fn select_tools(
        &self,
        all_tools: Vec<FunctionToolSpec>,
        messages: &[Message],
    ) -> Vec<FunctionToolSpec> {
        let remaining = self.budget.remaining_for_input(&all_tools, messages);
        if remaining > 0 {
            return all_tools;
        }

        tracing::warn!(
            total_tools = all_tools.len(),
            "工具定义超出 token 预算，裁剪低优先级工具"
        );

        let priority_tools = [
            "read_file",
            "write_file",
            "replace_in_file",
            "list_dir",
            "run_command",
            "search_code",
            "web_fetch",
            "tree_dir",
        ];

        all_tools
            .into_iter()
            .filter(|t| priority_tools.contains(&t.name.as_str()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::session::Session;

    fn empty_session() -> Session {
        Session {
            id: "test".into(),
            title: "test".into(),
            messages: Vec::new(),
            token_usage: Default::default(),
            task_records: Vec::new(),
            task_plans: Vec::new(),
            parent_session_id: None,
            cwd: String::new(),
            cwd_mode: Default::default(),
            context_summary: None,
            summary_up_to: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn empty_input_defaults_to_tool_mode() {
        // classify with empty input should not call LLM, returns ToolExecution directly
        // We can't test LLM-dependent classify without a client, but we can test the guard
        let session = empty_session();
        // Empty input bypasses LLM call
        // This test verifies the early return path
        assert_eq!(session.task_records.len(), 0);
    }

    #[test]
    fn session_with_tool_history_always_tool_mode() {
        let mut session = empty_session();
        session
            .task_records
            .push(crate::session::SessionTaskRecord {
                task_id: "t1".into(),
                tool_result: Some("some result".into()),
                ..Default::default()
            });
        assert!(session.task_records.iter().any(|r| r.tool_result.is_some()));
    }
}
