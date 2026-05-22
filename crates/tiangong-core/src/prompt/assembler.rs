//! Prompt 装配器
//!
//! 将各层内容按正确顺序组装为 AssembledPrompt。

use crate::agent_config::AgentConfig;
use crate::model::ToolSpec;
use crate::models_config::ModelsConfig;
use crate::session::{Message, MessageRole, Session, now_text};

use super::sections;
use super::types::AssembledPrompt;

/// Prompt 装配器
pub struct PromptAssembler {
    context_limit: usize,
}

impl PromptAssembler {
    pub fn new(context_limit: usize) -> Self {
        Self { context_limit }
    }

    /// 装配完整 prompt
    ///
    /// 输入：session（历史）、user_input（当前输入）、工具定义、配置
    /// 输出：AssembledPrompt，包含发送给 API 所需的全部数据
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        &self,
        session: &Session,
        user_input: &str,
        tools: Vec<ToolSpec>,
        models_config: &ModelsConfig,
        agent_config: &AgentConfig,
        memory_context: Option<&str>,
        team_context_message: Option<&Message>,
    ) -> AssembledPrompt {
        let _ = self.context_limit;

        // 1. 构建动态段（媒体能力、Skills、团队协作、自定义指令等）
        let system_block = sections::build_system_prompt_block(models_config, agent_config);
        let mut dynamic_sections: Vec<String> = system_block
            .dynamic_blocks
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        // 静态段中的自定义指令也需注入（身份和规则已在 build_provider_messages 中）
        for block in &system_block.static_blocks {
            if !block.is_empty() && !block.contains("天工智能助手") && !block.starts_with("规则")
            {
                dynamic_sections.push(block.clone());
            }
        }

        // 2. System Context（环境事实）
        let system_context = sections::build_system_context(session);

        // 3. 历史消息（摘要之后的完整消息）
        let history_messages = session.messages[session.summary_up_to..].to_vec();

        // 4. 记忆上下文（recall_memory 检索结果）
        let memory_prefix_message =
            memory_context
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|ctx| Message {
                    id: scru128::new().to_string(),
                    role: MessageRole::Tool,
                    content: format!(
                        "<memory-recall>\n{ctx}\n</memory-recall>\n\
                    请基于以上 recall_memory 检索结果继续完成用户原始目标；\
                    不要再次调用 recall_memory，除非用户提出新的历史查询。"
                    ),
                    reasoning_content: String::new(),
                    reasoning_signature: None,
                    worker_id: None,
                    media: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tool_name: Some("recall_memory".to_string()),
                    tool_result_is_error: false,
                    compact: false,
                    created_at: now_text(),
                });

        // 5. context_summary 作为 Tool 消息注入对话流（不进系统提示词）
        let context_summary_message = session
            .context_summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|summary| Message {
                id: scru128::new().to_string(),
                role: MessageRole::Tool,
                content: format!(
                    "<context-summary>\n{summary}\n</context-summary>\n\
                    请将以上摘要视为此前多轮对话的压缩上下文。"
                ),
                reasoning_content: String::new(),
                reasoning_signature: None,
                worker_id: None,
                media: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: Some("context_summary".to_string()),
                tool_result_is_error: false,
                compact: false,
                created_at: now_text(),
            });

        // 6. System Attachments（MCP 工具摘要等系统级工具上下文）
        let attachment_messages = build_attachments(agent_config);

        AssembledPrompt {
            system_prompt: dynamic_sections.join("\n\n"),
            system_context,
            user_context: Vec::new(),
            memory_prefix_message,
            team_context_message: team_context_message.cloned(),
            history_messages,
            attachment_messages,
            context_summary_message,
            user_input: user_input.to_string(),
            tools,
        }
    }
}

/// 构建 Attachment Messages（高波动内容，不进 system prompt 主干）
fn build_attachments(agent_config: &AgentConfig) -> Vec<Message> {
    let mut attachments = Vec::new();

    // MCP 工具摘要（从缓存读取，不注入 system prompt）
    if let Some(mcp_text) = crate::mcp::build_mcp_tools_system_prompt(24) {
        attachments.push(Message {
            id: scru128::new().to_string(),
            role: crate::session::MessageRole::Tool,
            content: format!(
                "<system-reminder>\n<mcp-tools>\n{mcp_text}\n</mcp-tools>\n</system-reminder>"
            ),
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: Some("mcp_tools_summary".to_string()),
            tool_result_is_error: false,
            compact: false,
            created_at: crate::session::now_text(),
        });
    }

    let _ = agent_config; // 后续可扩展：skill 变化通知、诊断结果等

    attachments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_basic() {
        let assembler = PromptAssembler::new(32768);
        let session = Session::new("测试");
        let result = assembler.assemble(
            &session,
            "你好",
            Vec::new(),
            &ModelsConfig::default(),
            &crate::agent_config::AgentConfig::default(),
            None,
            None,
        );

        assert_eq!(result.user_input, "你好");
        assert!(result.system_context.iter().any(|c| c.contains("工作目录")));
    }
}
