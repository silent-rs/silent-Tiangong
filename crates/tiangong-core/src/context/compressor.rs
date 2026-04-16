use anyhow::Result;

use crate::model::{ModelClient, ModelRequest, SingleProviderClient};
use crate::session::{Message, MessageRole, Session, now_text};

/// 上下文压缩器
///
/// 采用滚动摘要策略：保留最近 N 轮完整对话，对更早的消息（含旧摘要）
/// 折叠为新摘要。摘要持久化到 Session，支持无限对话延伸。
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

    /// 更新 session 的滚动摘要
    ///
    /// 如果消息数量足够多（超过 keep_recent_turns），将早期消息（含旧摘要）
    /// 折叠为新摘要，更新 session.context_summary 和 summary_up_to。
    ///
    /// 返回 true 表示执行了压缩，false 表示无需压缩。
    pub fn update_summary(
        &self,
        session: &mut Session,
        client: &SingleProviderClient,
    ) -> Result<bool> {
        let split_point = self.find_split_point(&session.messages);
        if split_point == 0 {
            return Ok(false);
        }

        // 已被摘要覆盖的部分不需要重新处理
        if split_point <= session.summary_up_to {
            return Ok(false);
        }

        // 需要新增摘要的消息范围：从上次摘要覆盖点到新分割点
        let new_messages_start = session.summary_up_to;
        let new_messages = &session.messages[new_messages_start..split_point];

        if new_messages.is_empty() {
            return Ok(false);
        }

        // 折叠：旧摘要 + 新溢出消息 → 新摘要
        let summary =
            self.fold_summary(session.context_summary.as_deref(), new_messages, client)?;

        tracing::info!(
            old_summary_up_to = session.summary_up_to,
            new_summary_up_to = split_point,
            new_messages_count = new_messages.len(),
            summary_len = summary.len(),
            "滚动摘要已更新"
        );

        session.context_summary = Some(summary);
        session.summary_up_to = split_point;
        Ok(true)
    }

    /// 构建发送给 LLM 的上下文消息列表
    ///
    /// 结构：[摘要系统消息(可选)] + [最近 N 轮完整消息]
    pub fn build_context(&self, session: &Session) -> Vec<Message> {
        let split_point = if session.summary_up_to > 0 {
            // 使用已有摘要覆盖点
            session.summary_up_to
        } else {
            // 没有摘要时返回全部消息
            0
        };

        let recent_messages = &session.messages[split_point..];
        let mut context = Vec::new();

        // 注入摘要
        if let Some(summary) = &session.context_summary {
            context.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: format!("[早期对话摘要]\n{summary}"),
                reasoning_content: String::new(),
                worker_id: None,
                media: Vec::new(),
                created_at: now_text(),
            });
        }

        context.extend_from_slice(recent_messages);
        context
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

    /// 将旧摘要 + 新消息折叠为新摘要
    fn fold_summary(
        &self,
        old_summary: Option<&str>,
        new_messages: &[Message],
        client: &SingleProviderClient,
    ) -> Result<String> {
        let mut input_text = String::new();

        // 旧摘要作为上文
        if let Some(summary) = old_summary {
            input_text.push_str(&format!("[已有摘要]\n{summary}\n\n[新增对话]\n"));
        }

        // 新消息
        for msg in new_messages {
            let role_label = match msg.role {
                MessageRole::User => "用户",
                MessageRole::Assistant => "助手",
                MessageRole::System => "系统",
            };
            let content = if msg.content.chars().count() > 2000 {
                let truncated: String = msg.content.chars().take(2000).collect();
                format!("{truncated}...(已截断)")
            } else {
                msg.content.clone()
            };
            input_text.push_str(&format!("[{role_label}]: {content}\n"));
        }

        let prompt = format!(
            "请将以下内容压缩为简洁的对话摘要。要求：\n\
             1. 保留所有关键信息、决策结论和重要数据\n\
             2. 保留用户的核心需求和偏好\n\
             3. 保留已执行的操作及其结果\n\
             4. 如果有已有摘要，将其与新增内容合并为统一摘要\n\
             5. 去除冗余的中间过程和重复内容\n\
             6. 直接输出摘要内容，不要加前缀说明\n\n\
             {input_text}"
        );

        let req = ModelRequest {
            session_title: String::new(),
            user_input: prompt,
            context: Vec::new(),
            assembled_system_prompt: None,
            thinking: None,
        };
        let resp = client.complete(&req)?;
        Ok(resp.text)
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
    // 按 Assistant+System 配对分组为"轮次"
    let mut rounds: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < loop_messages.len() {
        let start = i;
        if i < loop_messages.len() && loop_messages[i].role == MessageRole::Assistant {
            i += 1;
        }
        while i < loop_messages.len() && loop_messages[i].role == MessageRole::System {
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
        };
        let content = if msg.content.chars().count() > 1000 {
            let truncated: String = msg.content.chars().take(1000).collect();
            format!("{truncated}...(已截断)")
        } else {
            msg.content.clone()
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
        assembled_system_prompt: None,
        thinking: None,
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
        role: MessageRole::System,
        content: format!("[前 {compress_rounds} 轮执行摘要]\n{summary}"),
        reasoning_content: String::new(),
        worker_id: None,
        media: Vec::new(),
        created_at: now_text(),
    }];
    result.extend_from_slice(recent_messages);
    Ok(result)
}
