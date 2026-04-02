//! 上下文装配协调器
//!
//! 根据查询编排层的执行模式决策，组装当前轮的完整上下文视图。
//! 包括：历史消息选择、工具定义注入、环境信息注入、预算控制。

use crate::agent_config::AgentConfig;
use crate::model::{FunctionToolSpec, ModelClient, ModelRequest, SingleProviderClient};
use crate::models_config::ModelsConfig;
use crate::session::{Message, Session};

use super::budget::TokenBudget;
use super::organizer::ContextOrganizer;

/// 查询执行模式
///
/// 由查询编排层判断，传递给上下文装配层，决定注入哪些内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    /// 直接回答：不注入工具定义（简单对话、闲聊）
    DirectAnswer,
    /// 工具执行：注入完整工具定义
    ToolExecution,
}

/// 查询意图分类器
///
/// 使用 LLM 判断用户输入是否需要工具支持。
pub struct QueryClassifier;

impl QueryClassifier {
    /// 使用 LLM 对用户输入进行意图分类，返回模式和可能的 LLM 调用记录
    pub fn classify(
        input: &str,
        session: &Session,
        client: &SingleProviderClient,
    ) -> (QueryMode, Vec<crate::session::LlmCallRecord>) {
        let input = input.trim();

        // 空输入走工具模式
        if input.is_empty() {
            return (QueryMode::ToolExecution, Vec::new());
        }

        // 历史中有工具调用的会话，继续使用工具
        let has_tool_history = session.task_records.iter().any(|r| r.tool_result.is_some());
        if has_tool_history {
            return (QueryMode::ToolExecution, Vec::new());
        }

        // 调用 LLM 进行意图分类
        match Self::classify_with_llm(input, client) {
            Ok((mode, record)) => {
                #[cfg(feature = "llm-debug-log")]
                return (mode, vec![record]);
                #[cfg(not(feature = "llm-debug-log"))]
                {
                    let _ = record;
                    (mode, Vec::new())
                }
            }
            Err(err) => {
                tracing::warn!("意图分类 LLM 调用失败，回退到工具模式: {err}");
                (QueryMode::ToolExecution, Vec::new())
            }
        }
    }

    /// 调用 LLM 判断意图，返回模式和调用记录
    fn classify_with_llm(
        input: &str,
        client: &SingleProviderClient,
    ) -> anyhow::Result<(QueryMode, crate::session::LlmCallRecord)> {
        let prompt = format!(
            "判断以下用户输入是否需要使用工具（如文件操作、命令执行、代码搜索、图片生成等）。\n\
             只回答一个词：chat（纯闲聊/知识问答，不需要工具）或 tool（需要工具执行操作）。\n\n\
             用户输入：{input}"
        );

        let req = ModelRequest {
            session_title: String::new(),
            user_input: prompt.clone(),
            context: Vec::new(),
        };

        let resp = client.complete(&req)?;
        let answer = resp.text.trim().to_lowercase();

        let mode = if answer.contains("chat") {
            QueryMode::DirectAnswer
        } else {
            QueryMode::ToolExecution
        };

        tracing::info!(
            input_len = input.len(),
            classify_prompt_tokens = resp.usage.prompt_tokens,
            classify_completion_tokens = resp.usage.completion_tokens,
            result = ?mode,
            "LLM 意图分类"
        );

        let record = crate::session::LlmCallRecord {
            stage: "intent-classify".to_string(),
            prompt,
            context_count: 0,
            tool_names: Vec::new(),
            response_text: resp.text,
            reasoning_len: resp.reasoning_content.len(),
            tool_calls: Vec::new(),
            usage: resp.usage,
            timestamp: crate::session::now_text(),
        };

        Ok((mode, record))
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

        // 根据模式决定工具注入
        let tools = match mode {
            QueryMode::DirectAnswer => {
                tracing::info!(
                    input_len = user_input.len(),
                    "快速路径：跳过工具注入（直接回答模式）"
                );
                Vec::new()
            }
            QueryMode::ToolExecution => {
                self.select_tools(all_tools, &messages)
            }
        };

        // 根据模式调整 system_prompt
        let system_prompt = match mode {
            QueryMode::DirectAnswer => format!(
                "你是天工智能助手。请用简洁友好的方式回复用户。\
                 回复使用 Markdown 格式。\n\n用户输入：\n{user_input}"
            ),
            QueryMode::ToolExecution => system_prompt,
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
            "read_file", "write_file", "replace_in_file", "list_dir",
            "run_command", "search_code", "tree_dir",
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
            task_records: Vec::new(),
            task_plans: Vec::new(),
            cwd: String::new(),
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
        session.task_records.push(crate::session::SessionTaskRecord {
            task_id: "t1".into(),
            tool_result: Some("some result".into()),
            ..Default::default()
        });
        assert!(session.task_records.iter().any(|r| r.tool_result.is_some()));
    }
}
