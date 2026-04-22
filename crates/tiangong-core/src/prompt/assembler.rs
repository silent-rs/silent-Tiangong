//! Prompt 装配器
//!
//! 将各层内容按正确顺序组装为 AssembledPrompt。

use crate::agent_config::AgentConfig;
use crate::context::organizer::ContextOrganizer;
use crate::model::FunctionToolSpec;
use crate::models_config::ModelsConfig;
use crate::session::{Message, Session};

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
    pub fn assemble(
        &self,
        session: &Session,
        user_input: &str,
        tools: Vec<FunctionToolSpec>,
        models_config: &ModelsConfig,
        agent_config: &AgentConfig,
        loop_context: &[Message],
    ) -> AssembledPrompt {
        // 1. 构建 System Prompt（静态 + 动态块）
        let system_block = sections::build_system_prompt_block(models_config, agent_config);
        let system_prompt = system_block.to_text();

        // 2. System Context（环境事实）
        let system_context = sections::build_system_context();

        // 3. User Context（用户偏好/记忆，system-reminder 方式）
        let workspace_id = std::env::current_dir()
            .ok()
            .map(|p| tiangong_memory::types::workspace_id_from_path(&p));
        let user_context = sections::build_user_context(&session.id, workspace_id.as_deref());

        // 4. 历史消息（经过裁剪/压缩）
        let organizer = ContextOrganizer::new(self.context_limit).with_keep_recent_turns(6);
        let mut history_messages = organizer.build_context(session);

        // 追加 loop_context（当前事件循环的中间消息）
        history_messages.extend(loop_context.iter().cloned());

        // 5. Attachment Messages（MCP 工具摘要等高波动内容）
        let attachment_messages = build_attachments(agent_config);

        AssembledPrompt {
            system_prompt,
            system_context,
            user_context,
            history_messages,
            attachment_messages,
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
            role: crate::session::MessageRole::System,
            content: format!("<mcp-tools>\n{mcp_text}\n</mcp-tools>"),
            reasoning_content: String::new(),
            worker_id: None,
            media: Vec::new(),
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
            &[],
        );

        assert!(result.system_prompt.contains("天工"));
        assert!(result.system_prompt.contains("规则"));
        assert_eq!(result.user_input, "你好");
        assert!(result.system_context.iter().any(|c| c.contains("工作目录")));
    }
}
