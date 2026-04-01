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

        // 包含工具触发意图的关键词 → 工具模式
        let lower = input.to_lowercase();
        let tool_indicators = [
            // 文件操作
            "文件", "目录", "代码", "搜索", "路径",
            // 执行操作
            "执行", "运行", "命令", "终端", "shell",
            // 文件修改
            "创建", "删除", "修改", "编辑", "写入", "读取", "查看", "打开",
            // 多媒体
            "图片", "生成图", "画", "视频", "语音", "播放", "录音",
            // 工具管理
            "安装", "卸载", "skill", "mcp", "@",
            // 开发
            "编译", "构建", "build", "deploy", "git", "cargo", "npm", "yarn",
            // 网络
            "下载", "上传", "curl", "wget", "api",
        ];
        for indicator in &tool_indicators {
            if lower.contains(indicator) {
                return QueryMode::ToolExecution;
            }
        }

        // 短输入且无工具指示 → 直接回答
        if input.chars().count() < 200 {
            return QueryMode::DirectAnswer;
        }

        // 默认走工具模式（保守策略）
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
        assert_eq!(QueryClassifier::classify("今天天气怎么样？", &session), QueryMode::DirectAnswer);
        assert_eq!(QueryClassifier::classify("帮我解释一下什么是 Rust", &session), QueryMode::DirectAnswer);
    }

    #[test]
    fn tool_keywords_trigger_tool_mode() {
        let session = empty_session();
        assert_eq!(QueryClassifier::classify("读取 config.toml 文件", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("执行 cargo build", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("搜索代码中的 TODO", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("帮我生成图片", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("创建一个新文件", &session), QueryMode::ToolExecution);
        assert_eq!(QueryClassifier::classify("git status", &session), QueryMode::ToolExecution);
    }

    #[test]
    fn empty_input_defaults_to_tool_mode() {
        let session = empty_session();
        assert_eq!(QueryClassifier::classify("", &session), QueryMode::ToolExecution);
    }

    #[test]
    fn long_input_without_keywords_defaults_to_tool_mode() {
        let session = empty_session();
        let long_input = "a".repeat(250);
        assert_eq!(QueryClassifier::classify(&long_input, &session), QueryMode::ToolExecution);
    }

    #[test]
    fn session_with_tool_history_always_tool_mode() {
        let mut session = empty_session();
        session.task_records.push(crate::session::SessionTaskRecord {
            task_id: "t1".into(),
            tool_result: Some("some result".into()),
            ..Default::default()
        });
        // 即使输入看起来是闲聊，有工具历史的会话也走工具模式
        assert_eq!(QueryClassifier::classify("你好", &session), QueryMode::ToolExecution);
    }
}
