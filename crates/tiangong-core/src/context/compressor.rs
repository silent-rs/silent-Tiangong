use anyhow::Result;

use crate::model::{ModelClient, ModelRequest, SingleProviderClient, TokenUsage};
use crate::session::{ContentBlock, Message, MessagePhase, MessageRole, Session, now_text};

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
        if split_point == 0 {
            return Ok(CompressionUpdate::default());
        }

        if split_point <= session.summary_up_to {
            return Ok(CompressionUpdate::default());
        }

        let new_messages = &session.messages[session.summary_up_to..split_point];

        if new_messages.is_empty() {
            return Ok(CompressionUpdate::default());
        }

        let (summary, usage) = self.fold_summary(session, new_messages, client)?;

        tracing::info!(
            old_summary_up_to = session.summary_up_to,
            new_summary_up_to = split_point,
            messages_count = new_messages.len(),
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

    /// 构建发送给 LLM 的上下文消息列表
    pub fn build_context(&self, session: &Session) -> Vec<Message> {
        let split_point = if session.summary_up_to > 0 {
            session.summary_up_to
        } else {
            0
        };
        session.messages[split_point..].to_vec()
    }

    /// 查找分割点：保留最近 N 轮对话（一轮 = 一次 user 消息及其后续消息）
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

    /// 折叠摘要：将待压缩消息作为对话流发送给 LLM
    ///
    /// context_summary 和压缩指令作为 user 消息附加在对话流中，
    /// system prompt 保持默认以复用 KV cache。
    fn fold_summary(
        &self,
        session: &Session,
        new_messages: &[Message],
        client: &SingleProviderClient,
    ) -> Result<(String, TokenUsage)> {
        let mut context = Vec::new();

        let mut instruction = String::from(
            "请将以下对话历史压缩为简洁摘要。保留关键信息、决策结论和重要数据，去除冗余的中间过程和重复内容。直接输出摘要内容。",
        );
        if let Some(summary) = session
            .context_summary
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            instruction.push_str(&format!("\n\n[已有摘要]\n{summary}"));
        }

        context.push(Message {
            id: scru128::new().to_string(),
            role: MessageRole::User,
            content: vec![ContentBlock::text(instruction)],
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            elapsed_ms: None,
            turn_status: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            phase: MessagePhase::Normal,
            created_at: crate::session::now_text(),
        });

        context.extend(new_messages.iter().cloned());

        let req = ModelRequest {
            session_title: String::new(),
            user_input: String::new(),
            context,
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
            include_media: false,
        };
        let resp = client.complete(&req)?;
        Ok((resp.text, resp.usage))
    }
}

/// 压缩 ReAct 循环内的 loop_messages
///
/// 当循环内消息过多时，对早期轮次的工具调用和结果进行摘要，
/// 保留最近 N 轮的完整信息。
pub fn compress_loop_messages(
    loop_messages: &[Message],
    keep_recent: usize,
    client: &SingleProviderClient,
) -> Result<Vec<Message>> {
    // 按 Assistant + Tool/System 配对分组为"轮次"
    let mut rounds: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < loop_messages.len() {
        let start = i;
        if i < loop_messages.len() && loop_messages[i].role == MessageRole::Assistant {
            i += 1;
        }
        while i < loop_messages.len()
            && matches!(
                loop_messages[i].role,
                MessageRole::System | MessageRole::Tool
            )
        {
            i += 1;
        }
        if i > start {
            rounds.push((start, i));
        } else {
            i += 1;
        }
    }

    if rounds.len() <= keep_recent {
        return Ok(loop_messages.to_vec());
    }

    let compress_rounds = rounds.len() - keep_recent;
    let compress_end = rounds[compress_rounds - 1].1;
    let early_messages = &loop_messages[..compress_end];
    let recent_messages = &loop_messages[compress_end..];

    let mut text = String::new();
    for msg in early_messages {
        let label = match msg.role {
            MessageRole::Assistant => "Agent",
            MessageRole::System => "工具结果",
            MessageRole::User => "用户",
            MessageRole::Tool => "工具结果",
        };
        let content_text = msg.text_content();
        let content = if content_text.chars().count() > 1000 {
            let truncated: String = content_text.chars().take(1000).collect();
            format!("{truncated}...(已截断)")
        } else {
            content_text.clone()
        };
        text.push_str(&format!("[{label}]: {content}\n"));
    }

    let prompt = format!(
        "请将以下 Agent 执行过程压缩为简洁摘要。要求：\n\
         1. 保留每个工具调用的名称和关键结果\n\
         2. 保留成功/失败状态\n\
         3. 保留发现的重要信息\n\
         4. 去除工具输出的原始数据细节\n\
         5. 直接输出摘要，不要前缀说明\n\n\
         执行过程：\n{text}"
    );

    let req = ModelRequest {
        session_title: String::new(),
        user_input: prompt,
        context: Vec::new(),
        thinking: None,
        reasoning_effort: None,
        thinking_disabled: false,
        include_media: false,
    };

    let summary = match client.complete(&req) {
        Ok(resp) => resp.text,
        Err(err) => {
            tracing::warn!("循环内消息摘要失败，保留原始消息：{err}");
            return Ok(loop_messages.to_vec());
        }
    };

    let mut result = vec![Message {
        id: scru128::new().to_string(),
        role: MessageRole::Tool,
        content: vec![ContentBlock::text(format!(
            "[前 {compress_rounds} 轮执行摘要]\n{summary}"
        ))],
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        elapsed_ms: None,
        turn_status: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: Some("loop_summary".to_string()),
        tool_result_is_error: false,
        compact: true,
        phase: MessagePhase::Normal,
        created_at: now_text(),
    }];
    result.extend_from_slice(recent_messages);
    Ok(result)
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
