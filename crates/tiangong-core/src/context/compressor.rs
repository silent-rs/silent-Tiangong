use anyhow::Result;

use crate::model::SingleProviderClient;
use crate::session::{Message, MessageRole, now_text};

/// 上下文压缩策略
#[derive(Debug, Clone, Copy)]
pub enum CompressionStrategy {
    /// LLM 摘要压缩：对早期消息生成摘要，保留最近 N 轮完整对话
    LlmSummary,
    /// 滑动窗口截断：直接丢弃最早消息
    SlidingWindow,
}

/// 上下文压缩器
///
/// 当消息总 token 超过模型 context_limit 的 80% 时触发压缩，
/// 优先使用 LLM 摘要压缩，回退到滑动窗口截断。
pub struct ContextCompressor {
    /// 保留最近的完整对话轮数
    keep_recent_turns: usize,
}

impl Default for ContextCompressor {
    fn default() -> Self {
        Self {
            keep_recent_turns: 6,
        }
    }
}

impl ContextCompressor {
    pub fn new(keep_recent_turns: usize) -> Self {
        Self { keep_recent_turns }
    }

    /// 使用 LLM 摘要压缩早期消息
    ///
    /// 保留最近 N 轮完整对话，对更早的消息生成摘要
    pub fn compress_with_summary(
        &self,
        messages: &[Message],
        client: &SingleProviderClient,
    ) -> Result<Vec<Message>> {
        let split_point = self.find_split_point(messages);
        if split_point == 0 {
            return Ok(messages.to_vec());
        }

        let early_messages = &messages[..split_point];
        let recent_messages = &messages[split_point..];

        let summary = self.summarize_messages(early_messages, client)?;

        let mut result = vec![Message {
            id: scru128::new().to_string(),
            role: MessageRole::System,
            content: format!("[以下是早期对话摘要]\n{summary}"),
            reasoning_content: String::new(),
            created_at: now_text(),
        }];
        result.extend_from_slice(recent_messages);
        Ok(result)
    }

    /// 使用滑动窗口截断，直接丢弃最早消息
    pub fn compress_with_sliding_window(&self, messages: &[Message]) -> Vec<Message> {
        let split_point = self.find_split_point(messages);
        if split_point == 0 {
            return messages.to_vec();
        }
        messages[split_point..].to_vec()
    }

    /// 自动选择压缩策略
    ///
    /// 优先使用 LLM 摘要压缩，失败则回退到滑动窗口截断
    pub fn compress(
        &self,
        messages: &[Message],
        client: Option<&SingleProviderClient>,
    ) -> Result<Vec<Message>> {
        if let Some(client) = client {
            match self.compress_with_summary(messages, client) {
                Ok(result) => return Ok(result),
                Err(err) => {
                    tracing::warn!("LLM 摘要压缩失败，回退到滑动窗口：{err}");
                }
            }
        }
        Ok(self.compress_with_sliding_window(messages))
    }

    /// 查找分割点：保留最近 N 轮对话
    fn find_split_point(&self, messages: &[Message]) -> usize {
        let mut turn_count = 0;
        let mut split_point = messages.len();

        for (i, msg) in messages.iter().enumerate().rev() {
            if msg.role == MessageRole::User {
                turn_count += 1;
                if turn_count >= self.keep_recent_turns {
                    split_point = i;
                    break;
                }
            }
        }

        if split_point == messages.len() {
            return 0;
        }
        split_point
    }

    /// 使用 LLM 对消息列表生成摘要
    fn summarize_messages(
        &self,
        messages: &[Message],
        client: &SingleProviderClient,
    ) -> Result<String> {
        let mut conversation_text = String::new();
        for msg in messages {
            let role_label = match msg.role {
                MessageRole::User => "用户",
                MessageRole::Assistant => "助手",
                MessageRole::System => "系统",
            };
            conversation_text.push_str(&format!("[{role_label}]: {}\n", msg.content));
        }

        let prompt = format!(
            "请将以下对话历史压缩为简洁的摘要，保留关键信息和决策：\n\n{conversation_text}"
        );

        client.complete_lite(&prompt)
    }
}
