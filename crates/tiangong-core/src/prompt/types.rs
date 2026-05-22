//! Prompt 类型定义

use crate::model::ToolSpec;
use crate::session::Message;

/// System Prompt 块
///
/// 静态部分极少变化（缓存友好），动态部分按会话/轮次更新。
#[derive(Debug, Clone)]
pub struct SystemPromptBlock {
    /// 静态块：身份 + 规则（跨会话稳定）
    pub static_blocks: Vec<String>,
    /// 动态块：环境信息、MCP 指令等（会话级变化）
    pub dynamic_blocks: Vec<String>,
}

impl SystemPromptBlock {
    pub fn new() -> Self {
        Self {
            static_blocks: Vec::new(),
            dynamic_blocks: Vec::new(),
        }
    }

    /// 合并为最终 system prompt 文本
    pub fn to_text(&self) -> String {
        let mut parts = Vec::new();
        parts.extend(self.static_blocks.iter().cloned());
        parts.extend(self.dynamic_blocks.iter().cloned());
        parts.join("\n\n")
    }
}

impl Default for SystemPromptBlock {
    fn default() -> Self {
        Self::new()
    }
}

/// Prompt Section（动态块的组成单元）
#[derive(Debug, Clone)]
pub struct PromptSection {
    /// section 名称（调试用）
    pub name: String,
    /// section 内容
    pub content: String,
    /// 是否缓存（默认 true，即会话级稳定）
    pub cached: bool,
}

/// 装配完成的 Prompt
///
/// 包含发送给 API 所需的全部数据。
#[derive(Debug, Clone)]
pub struct AssembledPrompt {
    /// System Prompt 动态段（媒体能力、Skills、团队协作等）
    pub system_prompt: String,
    /// System Context（环境事实，注入对话流）
    pub system_context: Vec<String>,
    /// User Context（包装为 system-reminder，放在消息最前面）
    pub user_context: Vec<String>,
    /// 历史消息（经过裁剪/压缩）
    pub history_messages: Vec<Message>,
    /// Attachment Messages（MCP 工具摘要等）
    pub attachment_messages: Vec<Message>,
    /// 上下文压缩摘要（作为 Tool 消息注入对话流）
    pub context_summary_message: Option<Message>,
    /// 当前用户输入
    pub user_input: String,
    /// 工具定义
    pub tools: Vec<ToolSpec>,
}

impl AssembledPrompt {
    /// 构建最终的 system prompt 文本。
    ///
    /// 压缩后的历史摘要、运行环境和系统级工具上下文统一放入
    /// system prompt 动态段，避免它们作为普通上文消息参与对话链。
    pub fn final_system_prompt(&self) -> String {
        let mut parts = vec![self.system_prompt.clone()];
        parts.extend(self.system_context.iter().cloned());
        if let Some(attachments) = self.system_attachment_text() {
            parts.push(attachments);
        }
        parts.join("\n\n")
    }

    /// 构建 user context 文本（作为 system prompt 动态段注入）
    pub fn user_context_text(&self) -> Option<String> {
        if self.user_context.is_empty() {
            return None;
        }
        Some(format!(
            "## 用户偏好与记忆上下文\n{}\n\nIMPORTANT: this context may or may not be relevant to your tasks.",
            self.user_context.join("\n")
        ))
    }

    /// 构建系统级工具上下文文本（MCP 摘要、技能提示等）。
    pub fn system_attachment_text(&self) -> Option<String> {
        let sections = self
            .attachment_messages
            .iter()
            .filter_map(|message| {
                let content = message.content.trim();
                if content.is_empty() {
                    return None;
                }
                let name = message.tool_name.as_deref().unwrap_or("system_context");
                Some(format!("### {name}\n{content}"))
            })
            .collect::<Vec<_>>();

        if sections.is_empty() {
            None
        } else {
            Some(format!("## 系统级工具上下文\n{}", sections.join("\n\n")))
        }
    }

    /// 构建完整的消息列表（按文档顺序）
    ///
    /// 顺序：context_summary → attachments → history。
    /// 所有动态内容通过对话消息流传递，系统提示词保持稳定。
    pub fn build_messages(&self) -> Vec<Message> {
        let mut messages = Vec::new();
        // 1. 上下文压缩摘要（如存在）
        if let Some(ref msg) = self.context_summary_message {
            messages.push(msg.clone());
        }
        // 2. 系统级工具上下文（MCP 工具摘要等）
        messages.extend(self.attachment_messages.iter().cloned());
        // 3. 历史消息（包含用户输入）
        messages.extend(self.history_messages.iter().cloned());
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_block_merge() {
        let mut block = SystemPromptBlock::new();
        block.static_blocks.push("你是天工".into());
        block.dynamic_blocks.push("当前目录：/tmp".into());
        let text = block.to_text();
        assert!(text.contains("你是天工"));
        assert!(text.contains("当前目录"));
    }

    #[test]
    fn assembled_prompt_user_context() {
        let prompt = AssembledPrompt {
            system_prompt: String::new(),
            system_context: Vec::new(),
            user_context: vec!["偏好中文".into()],
            history_messages: Vec::new(),
            attachment_messages: Vec::new(),
            context_summary_message: None,
            user_input: "你好".into(),
            tools: Vec::new(),
        };
        let ctx = prompt.user_context_text().unwrap();
        assert!(ctx.contains("偏好中文"));
    }

    #[test]
    fn final_system_prompt_includes_dynamic_system_context() {
        let prompt = AssembledPrompt {
            system_prompt: "基础提示".into(),
            system_context: vec!["当前工作目录：/tmp".into()],
            user_context: vec!["偏好中文".into()],
            history_messages: Vec::new(),
            attachment_messages: vec![Message {
                id: "a1".into(),
                role: crate::session::MessageRole::Tool,
                content: "<mcp-tools>read_file</mcp-tools>".into(),
                reasoning_content: String::new(),
                reasoning_signature: None,
                worker_id: None,
                media: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: Some("mcp_tools_summary".into()),
                tool_result_is_error: false,
                compact: false,
                created_at: String::new(),
            }],
            context_summary_message: None,
            user_input: String::new(),
            tools: Vec::new(),
        };

        let system_prompt = prompt.final_system_prompt();

        assert!(system_prompt.contains("基础提示"));
        assert!(system_prompt.contains("当前工作目录"));
        assert!(!system_prompt.contains("偏好中文"));
        assert!(system_prompt.contains("mcp_tools_summary"));
    }

    #[test]
    fn assembled_prompt_no_user_context() {
        let prompt = AssembledPrompt {
            system_prompt: String::new(),
            system_context: Vec::new(),
            user_context: Vec::new(),
            history_messages: Vec::new(),
            attachment_messages: Vec::new(),
            context_summary_message: None,
            user_input: "你好".into(),
            tools: Vec::new(),
        };
        assert!(prompt.user_context_text().is_none());
    }

    #[test]
    fn build_messages_order() {
        let prompt = AssembledPrompt {
            system_prompt: String::new(),
            system_context: Vec::new(),
            user_context: vec!["ctx".into()],
            history_messages: vec![Message {
                id: "h1".into(),
                role: crate::session::MessageRole::User,
                content: "历史".into(),
                reasoning_content: String::new(),
                reasoning_signature: None,
                worker_id: None,
                media: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: None,
                tool_result_is_error: false,
                compact: false,
                created_at: String::new(),
            }],
            attachment_messages: Vec::new(),
            context_summary_message: None,
            user_input: "当前输入".into(),
            tools: Vec::new(),
        };
        let msgs = prompt.build_messages();
        // 无 context_summary、无 attachment，只保留历史（用户输入已在 history 中）
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "历史");
    }
}
