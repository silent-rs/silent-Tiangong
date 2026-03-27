use anyhow::Result;

use crate::model::{ModelClient, ModelRequest, SingleProviderClient};
use crate::session::{Message, MessageRole, now_text};

/// 上下文压缩器
///
/// 当对话历史过长时，对早期消息生成 LLM 摘要，保留最近 N 轮完整对话。
/// 如果 LLM 摘要失败，回退到滑动窗口截断。
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

    /// 自动压缩：优先 LLM 摘要，失败回退滑动窗口
    pub fn compress(
        &self,
        messages: &[Message],
        client: Option<&SingleProviderClient>,
    ) -> Result<Vec<Message>> {
        let split_point = self.find_split_point(messages);
        if split_point == 0 {
            return Ok(messages.to_vec());
        }

        if let Some(client) = client {
            match self.compress_with_summary(messages, split_point, client) {
                Ok(result) => return Ok(result),
                Err(err) => {
                    tracing::warn!("LLM 摘要压缩失败，回退到滑动窗口：{err}");
                }
            }
        }

        Ok(messages[split_point..].to_vec())
    }

    /// 使用 LLM 摘要压缩早期消息
    fn compress_with_summary(
        &self,
        messages: &[Message],
        split_point: usize,
        client: &SingleProviderClient,
    ) -> Result<Vec<Message>> {
        let early_messages = &messages[..split_point];
        let recent_messages = &messages[split_point..];

        let summary = self.summarize_messages(early_messages, client)?;

        let mut result = vec![Message {
            id: scru128::new().to_string(),
            role: MessageRole::System,
            content: format!("[早期对话摘要]\n{summary}"),
            reasoning_content: String::new(),
            created_at: now_text(),
        }];
        result.extend_from_slice(recent_messages);
        Ok(result)
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
            // 截断过长的单条消息，避免摘要请求本身超限
            let content = if msg.content.len() > 2000 {
                format!("{}...(已截断)", &msg.content[..2000])
            } else {
                msg.content.clone()
            };
            conversation_text.push_str(&format!("[{role_label}]: {content}\n"));
        }

        let prompt = format!(
            "请将以下对话历史压缩为简洁的摘要。要求：\n\
             1. 保留所有关键信息、决策结论和重要数据\n\
             2. 保留用户的核心需求和偏好\n\
             3. 保留已执行的操作及其结果\n\
             4. 去除冗余的中间过程和重复内容\n\
             5. 摘要使用第三人称陈述\n\
             6. 直接输出摘要内容，不要加前缀说明\n\n\
             对话历史：\n{conversation_text}"
        );

        let req = ModelRequest {
            session_title: String::new(),
            user_input: prompt,
            context: Vec::new(),
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
    let mut rounds: Vec<(usize, usize)> = Vec::new(); // (start, end) indices
    let mut i = 0;
    while i < loop_messages.len() {
        let start = i;
        // Assistant 消息
        if i < loop_messages.len() && loop_messages[i].role == MessageRole::Assistant {
            i += 1;
        }
        // 后续的 System 消息（工具结果）
        while i < loop_messages.len() && loop_messages[i].role == MessageRole::System {
            i += 1;
        }
        if i > start {
            rounds.push((start, i));
        } else {
            i += 1; // 跳过意外消息
        }
    }

    if rounds.len() <= keep_recent {
        return Ok(loop_messages.to_vec());
    }

    let compress_rounds = rounds.len() - keep_recent;
    let compress_end = rounds[compress_rounds - 1].1;
    let early_messages = &loop_messages[..compress_end];
    let recent_messages = &loop_messages[compress_end..];

    // 构建摘要文本
    let mut text = String::new();
    for msg in early_messages {
        let label = match msg.role {
            MessageRole::Assistant => "Agent",
            MessageRole::System => "工具结果",
            MessageRole::User => "用户",
        };
        let content = if msg.content.len() > 1000 {
            format!("{}...(已截断)", &msg.content[..1000])
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
        created_at: now_text(),
    }];
    result.extend_from_slice(recent_messages);
    Ok(result)
}
