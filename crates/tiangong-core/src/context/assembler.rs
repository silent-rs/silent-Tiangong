//! 上下文装配协调器
//!
//! 根据查询编排层的执行模式决策，组装当前轮的完整上下文视图。
//! 包括：历史消息选择、工具定义注入、环境信息注入、预算控制。

use crate::agent_config::AgentConfig;
use crate::model::FunctionToolSpec;
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
/// 根据用户输入和上下文判断执行模式，决定是否需要工具支持。
pub struct QueryClassifier;

impl QueryClassifier {
    /// 对用户输入进行意图分类
    pub fn classify(input: &str, session: &Session) -> QueryMode {
        let input = input.trim();

        // 空输入走工具模式（安全默认）
        if input.is_empty() {
            return QueryMode::ToolExecution;
        }

        // 历史中有工具调用的会话，倾向于继续使用工具
        let has_tool_history = session.task_records.iter().any(|r| {
            r.tool_result.is_some()
        });
        if has_tool_history {
            return QueryMode::ToolExecution;
        }

        // 反向判断：只有明确的简单闲聊/知识问答才走直接回答
        // 其余一律走工具模式（保守策略，避免遗漏）
        let char_count = input.chars().count();

        // 超过 20 个字符的输入可能包含任务意图，走工具模式
        if char_count > 20 {
            return QueryMode::ToolExecution;
        }

        // 短输入（≤ 20 字符）：只有纯粹的问候/闲聊才走直接回答
        let greeting_patterns = [
            "你好", "hello", "hi", "hey", "嗨", "哈喽",
            "早上好", "下午好", "晚上好", "早安", "晚安",
            "谢谢", "感谢", "thanks", "thank you",
            "再见", "拜拜", "bye",
        ];
        let lower = input.to_lowercase();
        for pattern in &greeting_patterns {
            if lower.contains(pattern) {
                return QueryMode::DirectAnswer;
            }
        }

        // 短输入但不是问候语 → 可能是简短指令，走工具模式
        QueryMode::ToolExecution
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
    pub fn assemble(
        &self,
        session: &Session,
        user_input: &str,
        all_tools: Vec<FunctionToolSpec>,
        system_prompt: String,
        models_config: &ModelsConfig,
        agent_config: &AgentConfig,
    ) -> AssembledContext {
        let mode = QueryClassifier::classify(user_input, session);

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
                self.select_tools(all_tools, &messages, models_config, agent_config)
            }
        };

        // 根据模式调整 system_prompt
        let system_prompt = match mode {
            QueryMode::DirectAnswer => user_input.to_string(),
            QueryMode::ToolExecution => system_prompt,
        };

        AssembledContext {
            messages,
            tools,
            system_prompt,
            mode,
        }
    }

    /// 选择要注入的工具（预算控制）
    fn select_tools(
        &self,
        all_tools: Vec<FunctionToolSpec>,
        messages: &[Message],
        _models_config: &ModelsConfig,
        _agent_config: &AgentConfig,
    ) -> Vec<FunctionToolSpec> {
        // 检查预算：如果所有工具加上消息已超出预算，裁剪低优先级工具
        let remaining = self.budget.remaining_for_input(&all_tools, messages);
        if remaining > 0 {
            return all_tools;
        }

        // 超预算时优先保留基础工具，裁剪 MCP/管理类工具
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
    use super::*;
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
    fn simple_greeting_is_direct_answer() {
        let session = empty_session();
        assert_eq!(QueryClassifier::classify("你好", &session), QueryMode::DirectAnswer);
        assert_eq!(QueryClassifier::classify("hello", &session), QueryMode::DirectAnswer);
        assert_eq!(QueryClassifier::classify("Hi", &session), QueryMode::DirectAnswer);
        assert_eq!(QueryClassifier::classify("谢谢", &session), QueryMode::DirectAnswer);
        assert_eq!(QueryClassifier::classify("早上好", &session), QueryMode::DirectAnswer);
    }

    #[test]
    fn short_non_greeting_is_tool_mode() {
        let session = empty_session();
        // 短输入但不是问候语 → 工具模式（保守策略）
        assert_eq!(QueryClassifier::classify("分析一下项目", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("看看目前的状态", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("帮我看看", &session), QueryMode::ToolExecution);
    }

    #[test]
    fn longer_input_always_tool_mode() {
        let session = empty_session();
        assert_eq!(QueryClassifier::classify("今天天气怎么样？明天会不会下雨？", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("帮我解释一下什么是 Rust 编程语言", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("读取 config.toml 文件", &session), QueryMode::ToolExecution);
    }

    #[test]
    fn empty_input_defaults_to_tool_mode() {
        let session = empty_session();
        assert_eq!(QueryClassifier::classify("", &session), QueryMode::ToolExecution);
    }

    #[test]
    fn session_with_tool_history_always_tool_mode() {
        let mut session = empty_session();
        session.task_records.push(crate::session::SessionTaskRecord {
            task_id: "t1".into(),
            tool_result: Some("some result".into()),
            ..Default::default()
        });
        assert_eq!(QueryClassifier::classify("你好", &session), QueryMode::ToolExecution);
    }
}
