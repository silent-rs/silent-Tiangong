use std::collections::HashSet;

use anyhow::Result;

use crate::model::{ModelClient, ModelRequest, SingleProviderClient, TokenUsage};
use crate::session::{Message, MessageRole, Session};

/// 上下文压缩器
///
/// 采用滚动摘要策略：保留最近 N 轮完整对话，对更早的消息（含旧摘要）
/// 折叠为新摘要。摘要持久化到 Session，支持无限对话延伸。
pub struct ContextCompressor {
    keep_recent_turns: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CompressionUpdate {
    pub compressed: bool,
    pub usage: TokenUsage,
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

    pub fn update_summary_with_usage(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
    ) -> Result<CompressionUpdate> {
        let split_point = self.find_split_point(&session.messages);
        self.update_summary_at_with_usage(session, client, split_point)
    }

    pub(crate) fn update_summary_at_with_usage(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
        split_point: usize,
    ) -> Result<CompressionUpdate> {
        let split_point = split_point.min(session.messages.len());
        if split_point <= session.summary_up_to {
            return Ok(CompressionUpdate::default());
        }

        let messages_count = session.messages[session.summary_up_to..split_point]
            .iter()
            .filter(|message| !message.model_excluded && message.role != MessageRole::System)
            .count();

        if messages_count == 0 {
            return Ok(CompressionUpdate::default());
        }

        let (summary, usage) = self.fold_summary(session, split_point, client)?;

        tracing::info!(
            old_summary_up_to = session.summary_up_to,
            new_summary_up_to = split_point,
            messages_count,
            summary_len = summary.len(),
            "滚动摘要已更新"
        );

        session.context_summary = Some(summary);
        session.summary_up_to = split_point;
        mark_compact_boundary(&mut session.messages, split_point);
        Ok(CompressionUpdate {
            compressed: true,
            usage,
        })
    }

    pub async fn update_summary_with_usage_async(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
    ) -> Result<CompressionUpdate> {
        let split_point = self.find_split_point(&session.messages);
        self.update_summary_at_with_usage_async(session, client, split_point, None)
            .await
    }

    pub(crate) async fn update_summary_from_context_async(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
        messages_to_compress: Vec<Message>,
    ) -> Result<CompressionUpdate> {
        let compressed_ids = messages_to_compress
            .iter()
            .map(|message| message.id.as_str())
            .collect::<HashSet<_>>();
        let split_point = session.messages[session.summary_up_to..]
            .iter()
            .position(|message| {
                message.role == MessageRole::User
                    && !message.model_excluded
                    && !compressed_ids.contains(message.id.as_str())
            })
            .map(|index| session.summary_up_to + index)
            .unwrap_or(session.messages.len());

        self.update_summary_at_with_usage_async(
            session,
            client,
            split_point,
            Some(messages_to_compress),
        )
        .await
    }

    pub(crate) async fn update_summary_at_with_usage_async(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
        split_point: usize,
        compression_context: Option<Vec<Message>>,
    ) -> Result<CompressionUpdate> {
        let split_point = split_point.min(session.messages.len());
        if split_point <= session.summary_up_to {
            return Ok(CompressionUpdate::default());
        }
        let messages_count = session.messages[session.summary_up_to..split_point]
            .iter()
            .filter(|message| !message.model_excluded && message.role != MessageRole::System)
            .count();
        if messages_count == 0 {
            return Ok(CompressionUpdate::default());
        }

        let (summary, usage) = self
            .fold_summary_async(session, split_point, compression_context, client)
            .await?;
        session.context_summary = Some(summary);
        session.summary_up_to = split_point;
        mark_compact_boundary(&mut session.messages, split_point);
        Ok(CompressionUpdate {
            compressed: true,
            usage,
        })
    }

    fn summary_request(
        session: &Session,
        split_point: usize,
        compression_context: Option<Vec<Message>>,
    ) -> ModelRequest {
        // 压缩请求是原对话的延续：先让模型读完整段待压缩对话，
        // 末尾追加压缩指令作为最后一条 User，头部注入 session 的 system prompt。
        // 不能 pop 队尾 Assistant——否则会丢失待压缩内容（一次提问场景下会丢失全部对话）。
        let body = compression_context.unwrap_or_else(|| {
            let split_point = split_point.min(session.messages.len());
            let start = session.summary_up_to.min(split_point);
            session.messages[start..split_point]
                .iter()
                .filter(|message| !message.model_excluded && message.role != MessageRole::System)
                .cloned()
                .collect::<Vec<_>>()
        });

        let mut context = Vec::with_capacity(body.len() + 2);
        if let Some(system) = session.system_prompt_message.as_ref() {
            context.push(system.clone());
        }
        context.extend(body);
        context.push(Message::new(
            MessageRole::User,
            Self::compress_instruction(session),
        ));

        ModelRequest {
            user_input: String::new(),
            context,
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
        }
    }

    /// 压缩指令：拼接已有摘要供模型增量更新。
    fn compress_instruction(session: &Session) -> String {
        let mut instruction = String::from(
            "请将以上对话历史压缩为简洁摘要。保留关键信息、决策结论和重要数据，去除冗余的中间过程和重复内容。不要回答用户，只输出摘要正文。",
        );
        if let Some(summary) = session
            .context_summary
            .as_deref()
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
        {
            instruction.push_str(&format!("\n\n[已有摘要]\n{summary}"));
        }
        instruction
    }

    /// 构建发送给 LLM 的上下文消息列表
    pub fn build_context(&self, session: &Session) -> Vec<Message> {
        let split_point = if session.summary_up_to > 0 {
            session.summary_up_to
        } else {
            0
        };
        session.messages[split_point..]
            .iter()
            .filter(|message| !message.model_excluded && message.role != MessageRole::System)
            .cloned()
            .collect()
    }

    /// 查找分割点：保留最近 N 轮对话（一轮 = 一次 user 消息及其后续消息）
    pub(crate) fn find_split_point(&self, messages: &[Message]) -> usize {
        let mut turn_count = 0;
        let mut split_point = messages.len();

        for (i, msg) in messages.iter().enumerate().rev() {
            if msg.role == MessageRole::User && !msg.model_excluded {
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

    /// 折叠摘要：取得边界内上下文，并在末尾追加内部压缩要求。
    fn fold_summary(
        &self,
        session: &Session,
        split_point: usize,
        client: &SingleProviderClient,
    ) -> Result<(String, TokenUsage)> {
        let req = Self::summary_request(session, split_point, None);
        let resp = client.complete(&req)?;
        Ok((resp.text, resp.usage))
    }

    async fn fold_summary_async(
        &self,
        session: &Session,
        split_point: usize,
        compression_context: Option<Vec<Message>>,
        client: &SingleProviderClient,
    ) -> Result<(String, TokenUsage)> {
        let req = Self::summary_request(session, split_point, compression_context);
        let resp = client.complete_async(&req).await?;
        Ok((resp.text, resp.usage))
    }
}

pub fn mark_compact_boundary(messages: &mut [Message], split_point: usize) {
    for message in messages.iter_mut() {
        message.compact = false;
    }
    if let Some(boundary) = split_point
        .checked_sub(1)
        .and_then(|index| messages.get_mut(index))
    {
        boundary.compact = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::MessageRole;

    fn user(text: &str) -> Message {
        Message::new(MessageRole::User, text)
    }

    fn assistant(text: &str) -> Message {
        Message::new(MessageRole::Assistant, text)
    }

    /// 单轮压缩（一次提问就触发）：body=[User, Assistant]，压缩指令应 append 到末尾，
    /// 原对话的 User/Assistant 必须完整保留，不能因 pop 队尾 Assistant 而丢失。
    #[test]
    fn summary_request_single_turn_keeps_assistant_and_appends_instruction() {
        let mut session = Session::new("test");
        session.messages = vec![user("你好"), assistant("你好，有什么可以帮你？")];

        let req = ContextCompressor::summary_request(&session, session.messages.len(), None);

        // System 缺省（session.system_prompt_message = None），故 context 应为
        // [User(你好), Assistant(回复), User(压缩指令)]。
        assert_eq!(req.context.len(), 3);
        assert_eq!(req.context[0].role, MessageRole::User);
        assert_eq!(req.context[0].text_content(), "你好");
        assert_eq!(req.context[1].role, MessageRole::Assistant);
        assert_eq!(req.context[1].text_content(), "你好，有什么可以帮你？");
        assert_eq!(req.context[2].role, MessageRole::User);
        assert!(req.context[2].text_content().contains("压缩为简洁摘要"));
    }

    /// system_prompt_message 存在时应作为 context 首条注入。
    #[test]
    fn summary_request_prepends_system_prompt_when_present() {
        let mut session = Session::new("test");
        session.system_prompt_message = Some(Message::new(MessageRole::System, "你是天工助手。"));
        session.messages = vec![user("问题"), assistant("回答")];

        let req = ContextCompressor::summary_request(&session, session.messages.len(), None);

        assert_eq!(req.context.len(), 4);
        assert_eq!(req.context[0].role, MessageRole::System);
        assert_eq!(req.context[0].text_content(), "你是天工助手。");
        assert_eq!(req.context[3].role, MessageRole::User);
        assert!(req.context[3].text_content().contains("压缩为简洁摘要"));
    }

    /// 已有摘要时应拼接到压缩指令末尾，供模型增量更新。
    #[test]
    fn summary_request_appends_existing_summary_to_instruction() {
        let mut session = Session::new("test");
        session.context_summary = Some("旧摘要内容".to_string());
        session.messages = vec![user("问题"), assistant("回答")];

        let req = ContextCompressor::summary_request(&session, session.messages.len(), None);

        let instruction = req.context.last().unwrap().text_content();
        assert!(instruction.contains("压缩为简洁摘要"));
        assert!(instruction.contains("[已有摘要]"));
        assert!(instruction.contains("旧摘要内容"));
    }

    /// 显式传入 compression_context 时应使用它而非 session.messages 切片。
    #[test]
    fn summary_request_uses_explicit_compression_context() {
        let mut session = Session::new("test");
        session.messages = vec![user("不应被使用"), assistant("不应被使用")];

        let explicit = vec![
            user("早期问题"),
            assistant("早期回答"),
            user("第二轮"),
            assistant("第二轮回答"),
        ];
        let req = ContextCompressor::summary_request(&session, 0, Some(explicit));

        // [U, A, U, A, User(指令)]
        assert_eq!(req.context.len(), 5);
        assert_eq!(req.context[0].text_content(), "早期问题");
        assert_eq!(req.context[3].role, MessageRole::Assistant);
        assert_eq!(req.context[3].text_content(), "第二轮回答");
    }
}
