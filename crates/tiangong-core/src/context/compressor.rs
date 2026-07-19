use anyhow::{Result, bail};

use crate::model::{ModelRequest, SingleProviderClient, StopReason, TokenUsage};
use crate::session::{ContentBlock, Message, MessagePhase, MessageRole, Session};
use crate::turn_context::TurnContext;

/// 判断消息是否为可压缩的有效消息。
fn is_compressible(message: &Message) -> bool {
    !message.model_excluded
        && message.role != MessageRole::System
        && message.phase != MessagePhase::CompressedResume
}

/// 使用 Session 和客户端快照执行压缩请求，不直接修改真实会话。
pub struct ContextCompressor {
    session: Session,
    client: SingleProviderClient,
}

#[derive(Debug, Clone)]
pub struct CompressionUpdate {
    pub summary: String,
    pub current_task: Option<String>,
    pub usage: TokenUsage,
    pub previous_summary_up_to: usize,
    pub summary_up_to: usize,
    pub boundary_message_id: String,
}

#[derive(Debug, Clone)]
pub struct CompressionError {
    pub message: String,
    pub usage: TokenUsage,
}

impl CompressionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            usage: TokenUsage::default(),
        }
    }

    fn with_usage(message: impl Into<String>, usage: TokenUsage) -> Self {
        Self {
            message: message.into(),
            usage,
        }
    }
}

impl std::fmt::Display for CompressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompressionError {}

impl ContextCompressor {
    pub fn new(ctx: &TurnContext) -> Self {
        Self {
            session: ctx.session.clone(),
            client: ctx.client.clone(),
        }
    }

    pub fn has_pending_messages(&self) -> bool {
        self.session.messages[self.session.summary_up_to..]
            .iter()
            .any(is_compressible)
    }

    /// 压缩当前摘要边界后的全部有效消息。
    pub async fn compress(
        self,
        max_output_tokens: u32,
        include_current_task: bool,
    ) -> std::result::Result<CompressionUpdate, CompressionError> {
        let previous_summary_up_to = self.session.summary_up_to;
        let split_point = self.session.messages.len();
        if split_point <= previous_summary_up_to {
            return Err(CompressionError::new(format!(
                "无可压缩消息：summary_up_to({previous_summary_up_to}) 已到达末尾({split_point})"
            )));
        }
        if !self.session.messages[previous_summary_up_to..split_point]
            .iter()
            .any(is_compressible)
        {
            return Err(CompressionError::new(format!(
                "待压缩范围 [{previous_summary_up_to}..{split_point}] 内无有效消息"
            )));
        }

        let boundary_message_id = self.session.messages[split_point - 1].id.clone();
        let request = Self::summary_request(
            &self.session,
            split_point,
            max_output_tokens,
            include_current_task,
        );
        let response = self
            .client
            .complete_async(&request)
            .await
            .map_err(|error| CompressionError::new(error.to_string()))?;
        let usage = response.usage.clone();
        if response.stop_reason == Some(StopReason::MaxTokens) {
            return Err(CompressionError::with_usage(
                "上下文压缩输出达到最大 token 限制，拒绝提交截断摘要",
                usage,
            ));
        }
        let (summary, current_task) = Self::parse_output(&response.text, include_current_task)
            .map_err(|error| CompressionError::with_usage(error, usage.clone()))?;

        Ok(CompressionUpdate {
            summary,
            current_task,
            usage,
            previous_summary_up_to,
            summary_up_to: split_point,
            boundary_message_id,
        })
    }

    fn summary_request(
        session: &Session,
        split_point: usize,
        max_output_tokens: u32,
        include_current_task: bool,
    ) -> ModelRequest {
        let split_point = split_point.min(session.messages.len());
        let start = session.summary_up_to.min(split_point);
        let mut context = Vec::with_capacity((split_point - start) + 2);
        if let Some(system) = session.system_prompt_message.as_ref() {
            context.push(system.clone());
        }
        context.extend(
            session.messages[start..split_point]
                .iter()
                .filter(|message| is_compressible(message))
                .cloned(),
        );
        context.push(Message::new(
            MessageRole::User,
            Self::compress_instruction(session, max_output_tokens, include_current_task),
        ));

        ModelRequest {
            user_input: String::new(),
            context,
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
            max_output_tokens: Some(max_output_tokens),
        }
    }

    fn compress_instruction(
        session: &Session,
        max_output_tokens: u32,
        include_current_task: bool,
    ) -> String {
        let existing_summary = session
            .context_summary
            .as_deref()
            .map(str::trim)
            .is_some_and(|summary| !summary.is_empty());
        let merge_hint = if existing_summary {
            "系统提示中已经包含此前对话摘要，请将它与以上新增对话合并；不要重复引用原摘要。\n"
        } else {
            ""
        };

        if include_current_task {
            format!(
                "请压缩以上对话历史。{merge_hint}\
                 总输出不得超过 {max_output_tokens} tokens，请在达到预算前主动结束。\n\
                 保留关键事实、决策、路径、错误和重要结果，删除重复内容、过程性描述和无效工具输出。\n\
                 不要回答用户，严格按以下顺序输出：\n\n\
                 [[CURRENT_TASK]]\n\
                 用户最近提问：<原文摘录>\n\
                 已完成：<已得到的关键结果>\n\
                 进行中：<当前事项，若无则填“无”>\n\
                 下一步：<下一步动作，若无则填“无”>\n\n\
                 [[SUMMARY]]\n\
                 <合并后的历史摘要>"
            )
        } else {
            format!(
                "请压缩以上对话历史。{merge_hint}\
                 总输出不得超过 {max_output_tokens} tokens，请在达到预算前主动结束。\n\
                 保留关键事实、决策、路径、错误和重要结果，删除重复内容、过程性描述和无效工具输出。\n\
                 不要回答用户，严格按以下格式输出：\n\n\
                 [[SUMMARY]]\n\
                 <合并后的历史摘要>"
            )
        }
    }

    fn parse_output(
        text: &str,
        include_current_task: bool,
    ) -> std::result::Result<(String, Option<String>), String> {
        const TASK_MARKER: &str = "[[CURRENT_TASK]]";
        const SUMMARY_MARKER: &str = "[[SUMMARY]]";
        let text = text.trim();
        if text.is_empty() {
            return Err("上下文压缩返回空内容，拒绝推进摘要边界".to_string());
        }

        if include_current_task {
            let task_start = text
                .find(TASK_MARKER)
                .ok_or_else(|| "上下文压缩缺少 CURRENT_TASK 段落".to_string())?;
            let summary_start = text
                .find(SUMMARY_MARKER)
                .ok_or_else(|| "上下文压缩缺少 SUMMARY 段落".to_string())?;
            if task_start >= summary_start {
                return Err(
                    "上下文压缩段落顺序错误：CURRENT_TASK 必须位于 SUMMARY 之前".to_string()
                );
            }
            let task = text[task_start + TASK_MARKER.len()..summary_start].trim();
            let summary = text[summary_start + SUMMARY_MARKER.len()..].trim();
            if task.is_empty() {
                return Err("上下文压缩返回空 CURRENT_TASK，拒绝推进摘要边界".to_string());
            }
            if summary.is_empty() {
                return Err("上下文压缩返回空摘要，拒绝推进摘要边界".to_string());
            }
            return Ok((summary.to_string(), Some(task.to_string())));
        }

        let summary = text
            .split_once(SUMMARY_MARKER)
            .map_or(text, |(_, summary)| summary)
            .trim();
        if summary.is_empty() {
            return Err("上下文压缩返回空摘要，拒绝推进摘要边界".to_string());
        }
        Ok((summary.to_string(), None))
    }
}

/// 将压缩结果应用到候选 Session，持久化成功后再替换真实 Session。
pub fn apply_compression(
    ctx: &mut TurnContext,
    update: &CompressionUpdate,
    account_usage_in_session: bool,
) -> Result<usize> {
    if ctx.session.summary_up_to != update.previous_summary_up_to {
        bail!(
            "压缩期间摘要边界已变化：expected={}, actual={}",
            update.previous_summary_up_to,
            ctx.session.summary_up_to
        );
    }
    let Some(boundary) = update
        .summary_up_to
        .checked_sub(1)
        .and_then(|index| ctx.session.messages.get(index))
    else {
        bail!("压缩结果边界无效：{}", update.summary_up_to);
    };
    if boundary.id != update.boundary_message_id {
        bail!("压缩期间消息边界已变化，拒绝提交过期结果");
    }

    let mut candidate = ctx.session.clone();
    candidate.context_summary = Some(update.summary.clone());
    candidate.summary_up_to = update.summary_up_to;
    mark_compact_boundary(&mut candidate.messages, update.summary_up_to);
    if account_usage_in_session {
        candidate.token_usage.accumulate(&update.usage);
        candidate.active_agent_current_tokens = 0;
        candidate.agent_current_tokens.clear();
    }
    let current_tokens = update.usage.completion_tokens;
    candidate.current_tokens = current_tokens;
    crate::react::context::rebuild_system_prompt_for_session(&mut candidate, &ctx.plugins);
    candidate
        .try_persist_to_disk()
        .map_err(anyhow::Error::msg)?;
    ctx.session = candidate;
    Ok(current_tokens)
}

/// 构造只进入下一次模型请求的任务续接消息。
pub fn build_compression_resume_message(current_task: &str) -> Message {
    let mut message = Message::new(MessageRole::User, "");
    message.content = vec![ContentBlock::model_instruction(format!(
        "以下是上下文压缩后的当前任务状态，请据此继续执行，不要重新询问用户：\n\n{current_task}"
    ))];
    message.phase = MessagePhase::Normal;
    message
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

    fn user(text: &str) -> Message {
        Message::new(MessageRole::User, text)
    }

    fn assistant(text: &str) -> Message {
        Message::new(MessageRole::Assistant, text)
    }

    #[test]
    fn summary_request_keeps_full_body_and_sets_budget() {
        let mut session = Session::new("test");
        session.system_prompt_message = Some(Message::new(MessageRole::System, "系统提示"));
        session.messages = vec![user("你好"), assistant("你好，有什么可以帮你？")];

        let request = ContextCompressor::summary_request(&session, 2, 10_000, true);

        assert_eq!(request.max_output_tokens, Some(10_000));
        assert_eq!(request.context.len(), 4);
        assert_eq!(request.context[0].role, MessageRole::System);
        assert_eq!(request.context[1].text_content(), "你好");
        assert_eq!(request.context[2].text_content(), "你好，有什么可以帮你？");
        let instruction = request.context[3].text_content();
        assert!(instruction.contains("不得超过 10000 tokens"));
        assert!(instruction.find("[[CURRENT_TASK]]") < instruction.find("[[SUMMARY]]"));
    }

    #[test]
    fn prompt_references_existing_summary_without_duplicating_it() {
        let mut session = Session::new("test");
        session.context_summary = Some("不应重复出现的旧摘要正文".to_string());

        let instruction = ContextCompressor::compress_instruction(&session, 50_000, true);

        assert!(instruction.contains("系统提示中已经包含此前对话摘要"));
        assert!(!instruction.contains("不应重复出现的旧摘要正文"));
    }

    #[test]
    fn parses_task_before_summary() {
        let text = "[[CURRENT_TASK]]\n用户最近提问：X\n下一步：Y\n\n[[SUMMARY]]\n历史摘要";

        let (summary, task) = ContextCompressor::parse_output(text, true).unwrap();

        assert_eq!(summary, "历史摘要");
        assert_eq!(task.as_deref(), Some("用户最近提问：X\n下一步：Y"));
    }

    #[test]
    fn rejects_empty_or_incomplete_output() {
        assert!(ContextCompressor::parse_output("", true).is_err());
        assert!(ContextCompressor::parse_output("[[SUMMARY]]\n摘要", true).is_err());
        assert!(ContextCompressor::parse_output("[[CURRENT_TASK]]\n任务", true).is_err());
        assert!(
            ContextCompressor::parse_output("[[CURRENT_TASK]]\n\n[[SUMMARY]]\n摘要", true).is_err()
        );
        assert!(
            ContextCompressor::parse_output("[[CURRENT_TASK]]\n任务\n[[SUMMARY]]", true).is_err()
        );
    }

    #[test]
    fn manual_output_accepts_plain_non_empty_summary() {
        let (summary, task) = ContextCompressor::parse_output("普通摘要", false).unwrap();

        assert_eq!(summary, "普通摘要");
        assert!(task.is_none());
    }

    #[test]
    fn resume_message_is_transient_normal_phase() {
        let message = build_compression_resume_message("继续任务");

        assert_eq!(message.role, MessageRole::User);
        assert_eq!(message.phase, MessagePhase::Normal);
        assert!(!message.model_excluded);
        assert!(matches!(
            &message.content[0],
            ContentBlock::ModelInstruction { text } if text.contains("继续任务")
        ));
    }
}
