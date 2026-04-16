//! Prompt 类型定义

use crate::model::FunctionToolSpec;
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
    /// System Prompt（合并后的文本）
    pub system_prompt: String,
    /// System Context 追加到 system prompt 尾部的环境信息
    pub system_context: Vec<String>,
    /// User Context（包装为 system-reminder，放在消息最前面）
    pub user_context: Vec<String>,
    /// 历史消息（经过裁剪/压缩）
    pub history_messages: Vec<Message>,
    /// Attachment Messages（技能列表、文件引用、诊断等）
    pub attachment_messages: Vec<Message>,
    /// 当前用户输入
    pub user_input: String,
    /// 工具定义
    pub tools: Vec<FunctionToolSpec>,
}

impl AssembledPrompt {
    /// 构建最终的 system prompt 文本（static + dynamic + system_context）
    pub fn final_system_prompt(&self) -> String {
        let mut parts = vec![self.system_prompt.clone()];
        parts.extend(self.system_context.iter().cloned());
        parts.join("\n\n")
    }

    /// 构建 user context 文本（包装为 system-reminder）
    pub fn user_context_text(&self) -> Option<String> {
        if self.user_context.is_empty() {
            return None;
        }
        Some(format!(
            "<system-reminder>\n{}\n\nIMPORTANT: this context may or may not be relevant to your tasks.\n</system-reminder>",
            self.user_context.join("\n")
        ))
    }

    /// 构建完整的消息列表（按文档顺序）
    ///
    /// 顺序：user_context_message → history → attachments
    /// 用户输入统一通过 session history 传递，不在此处追加。
    pub fn build_messages(&self) -> Vec<Message> {
        let mut messages = Vec::new();

        // User context 作为第一条 user 消息（system-reminder）
        if let Some(ctx_text) = self.user_context_text() {
            messages.push(Message {
                id: scru128::new().to_string(),
                role: crate::session::MessageRole::User,
                content: ctx_text,
                reasoning_content: String::new(),
                worker_id: None,
                media: Vec::new(),
                created_at: crate::session::now_text(),
            });
        }

        // 历史消息（包含用户输入）
        messages.extend(self.history_messages.iter().cloned());

        // Attachment messages
        messages.extend(self.attachment_messages.iter().cloned());

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
            user_input: "你好".into(),
            tools: Vec::new(),
        };
        let ctx = prompt.user_context_text().unwrap();
        assert!(ctx.contains("system-reminder"));
        assert!(ctx.contains("偏好中文"));
    }

    #[test]
    fn assembled_prompt_no_user_context() {
        let prompt = AssembledPrompt {
            system_prompt: String::new(),
            system_context: Vec::new(),
            user_context: Vec::new(),
            history_messages: Vec::new(),
            attachment_messages: Vec::new(),
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
                worker_id: None,
                media: Vec::new(),
                created_at: String::new(),
            }],
            attachment_messages: Vec::new(),
            user_input: "当前输入".into(),
            tools: Vec::new(),
        };
        let msgs = prompt.build_messages();
        // user_context → history（用户输入已在 history 中，不再单独追加）
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content.contains("system-reminder"));
        assert_eq!(msgs[1].content, "历史");
    }
}
