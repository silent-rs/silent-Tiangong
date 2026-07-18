use std::collections::HashSet;

use anyhow::Result;

use crate::model::{ModelClient, ModelRequest, SingleProviderClient, TokenUsage};
use crate::session::{Message, MessageRole, Session};

/// 上下文压缩器
///
/// 采用滚动摘要策略：压缩 `summary_up_to` 之后的所有消息（含旧摘要）
/// 折叠为新摘要。早期历史细节由摘要和 `CompressedResume` 合成消息承接。
#[derive(Default)]
pub struct ContextCompressor {
    /// 模型上下文上限（token）。压缩请求的输出预算据此动态计算，
    /// 避免 prompt 接近上限时 provider 因 prompt + max_tokens 超限而报错。
    context_limit: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CompressionUpdate {
    pub compressed: bool,
    pub usage: TokenUsage,
    /// 压缩时模型附带的「当前任务状态」。
    ///
    /// 当压缩发生在 turn 进行中（存在未完成的工具调用）时，用于构造
    /// `CompressedResume` 合成消息，避免重试请求失忆。turn 间滚动压缩
    /// 无进行中任务时为 `None`。
    pub current_task: Option<String>,
}

impl ContextCompressor {
    pub fn new(context_limit: usize) -> Self {
        Self { context_limit }
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

        let (summary, current_task, usage) = self.fold_summary(session, split_point, client)?;

        tracing::info!(
            old_summary_up_to = session.summary_up_to,
            new_summary_up_to = split_point,
            messages_count,
            summary_len = summary.len(),
            has_current_task = current_task.is_some(),
            "滚动摘要已更新"
        );

        // 摘要可能为空（模型输出空间不足等）。压缩的主目的是释放上下文空间，
        // 故仍推进边界；历史记忆由 CompressedResume 和未来的会话检索承接。
        session.context_summary = Some(summary);
        session.summary_up_to = split_point;
        mark_compact_boundary(&mut session.messages, split_point);
        Ok(CompressionUpdate {
            compressed: true,
            usage,
            current_task,
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

        let (summary, current_task, usage) = self
            .fold_summary_async(session, split_point, compression_context, client)
            .await?;

        // 摘要可能为空（见同步版注释），仍推进边界以释放空间。
        session.context_summary = Some(summary);
        session.summary_up_to = split_point;
        mark_compact_boundary(&mut session.messages, split_point);
        Ok(CompressionUpdate {
            compressed: true,
            usage,
            current_task,
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
            max_output_tokens: None,
        }
    }

    /// 压缩指令：拼接已有摘要供模型增量更新，并要求在末尾附上「当前任务状态」。
    ///
    /// 模型输出格式约定为：
    /// ```text
    /// <摘要正文>
    ///
    /// [[CURRENT_TASK]]
    /// <当前任务状态>
    /// ```
    /// `[[CURRENT_TASK]]` 之前是历史摘要，之后是压缩后用于续接的当前任务状态
    /// （最近用户提问、已完成结果、进行中工具、下一步）。解析时按此分隔符切分。
    fn compress_instruction(session: &Session) -> String {
        let mut instruction = String::from(
            "请将以上对话历史压缩为简洁摘要。保留关键信息、决策结论和重要数据，去除冗余的中间过程和重复内容。不要回答用户，只输出摘要正文。\n\n\
             输出末尾必须附上「当前任务状态」段落，用于压缩后继续执行。格式如下（严格遵守）：\n\
             <摘要正文>\n\
             \n\
             [[CURRENT_TASK]]\n\
             用户最近提问：<原文摘录>\n\
             已完成：<已得到的关键结果>\n\
             进行中：<当前正在执行的工具调用及其结果，若无则填「无」>\n\
             下一步：<推断的下一步动作>",
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

    /// 查找分割点：压缩 `summary_up_to` 之后的所有消息。
    ///
    /// 不再保留最近 N 轮——早期历史全部折叠进摘要，具体细节丢失由
    /// 「当前任务状态」合成消息（`CompressedResume`）和未来的会话检索承接。
    /// 调用方传入的 `messages` 长度即为压缩终点；`summary_up_to..len`
    /// 为空时由上层 `update_summary_at_with_usage*` 自然判为 noop。
    pub(crate) fn find_split_point(&self, messages: &[Message]) -> usize {
        messages.len()
    }

    /// 折叠摘要：取得边界内上下文，并在末尾追加内部压缩要求。
    fn fold_summary(
        &self,
        session: &Session,
        split_point: usize,
        client: &SingleProviderClient,
    ) -> Result<(String, Option<String>, TokenUsage)> {
        let req = Self::summary_request(session, split_point, None)
            .with_max_output_tokens(self.compression_output_budget());
        let resp = client.complete(&req)?;
        let (summary, current_task) = Self::split_summary_and_current_task(resp.text);
        Ok((summary, current_task, resp.usage))
    }

    async fn fold_summary_async(
        &self,
        session: &Session,
        split_point: usize,
        compression_context: Option<Vec<Message>>,
        client: &SingleProviderClient,
    ) -> Result<(String, Option<String>, TokenUsage)> {
        let req = Self::summary_request(session, split_point, compression_context)
            .with_max_output_tokens(self.compression_output_budget());
        let resp = client.complete_async(&req).await?;
        let (summary, current_task) = Self::split_summary_and_current_task(resp.text);
        Ok((summary, current_task, resp.usage))
    }

    /// 压缩请求的输出 token 预算。
    ///
    /// 压缩常在 prompt 达到 context_limit 的 95%（预防性压缩阈值）时触发，
    /// 此时 provider 允许的 completion ≈ context_limit 的 5%。声明的 max_tokens
    /// 不应超过这个剩余——部分 provider 严格校验 `prompt + max_tokens ≤ limit`，
    /// 声明过大（如写死 32k）会直接报错。
    ///
    /// 此处按 context_limit 的 5%（1/20）如实计算，不设下限：
    /// - 实际使用的 LLM ≥ 200k，5% = 10k+，写摘要充足；
    /// - 小上下文（<200k，不在使用建议范围）如实给出小预算，不人为抬高下限
    ///   导致声明超过实际剩余而报错。
    fn compression_output_budget(&self) -> u32 {
        const RATIO: u32 = 20; // 1/20 ≈ 5%，对齐预防性压缩阈值
        if self.context_limit == 0 {
            // context_limit 未知时保守取 50k（对齐 1M 上下文的 5%）。
            return 50_000;
        }
        (self.context_limit as u32) / RATIO
    }

    /// 将模型输出按 `[[CURRENT_TASK]]` 分隔符切分为（历史摘要，当前任务状态）。
    ///
    /// - 无分隔符：整体视为摘要，当前任务为 `None`（兼容模型未遵守格式的情况）。
    /// - 分隔符后为空：当前任务为 `None`。
    fn split_summary_and_current_task(text: String) -> (String, Option<String>) {
        const MARKER: &str = "[[CURRENT_TASK]]";
        match text.split_once(MARKER) {
            Some((summary, task)) => {
                let summary = summary.trim().to_string();
                let task = task.trim();
                if task.is_empty() {
                    (summary, None)
                } else {
                    (summary, Some(task.to_string()))
                }
            }
            None => (text.trim().to_string(), None),
        }
    }
}

/// 构造压缩后用于续接的合成消息（`CompressedResume`）。
///
/// 当压缩发生在 turn 进行中时注入，避免重试请求失忆。消息对前端不可见
///（`MessagePhase::CompressedResume`），但会发送给模型（`model_excluded: false`）。
/// 内容由模型在压缩时附带的「当前任务状态」提供；缺失时返回 `None`。
pub fn build_compressed_resume_message(current_task: &str) -> Message {
    use crate::session::{ContentBlock, MessagePhase, now_text};
    Message {
        id: scru128::new().to_string(),
        role: MessageRole::User,
        content: vec![ContentBlock::model_instruction(format!(
            "以下是上下文压缩后的当前任务状态，请据此继续执行，不要重新询问用户：\n\n{current_task}"
        ))],
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
        model_excluded: false,
        phase: MessagePhase::CompressedResume,
        created_at: now_text(),
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

    /// split_summary_and_current_task：正常分隔符切分。
    #[test]
    fn split_extracts_summary_and_current_task() {
        let text = "这是摘要正文。\n\n[[CURRENT_TASK]]\n用户最近提问：X\n进行中：工具Y";
        let (summary, task) = ContextCompressor::split_summary_and_current_task(text.to_string());
        assert_eq!(summary, "这是摘要正文。");
        assert_eq!(task.as_deref(), Some("用户最近提问：X\n进行中：工具Y"));
    }

    /// split_summary_and_current_task：无分隔符时整体视为摘要，task 为 None。
    #[test]
    fn split_without_marker_treats_all_as_summary() {
        let (summary, task) =
            ContextCompressor::split_summary_and_current_task("纯摘要无任务".to_string());
        assert_eq!(summary, "纯摘要无任务");
        assert!(task.is_none());
    }

    /// split_summary_and_current_task：分隔符后为空时 task 为 None。
    #[test]
    fn split_with_empty_task_returns_none() {
        let (summary, task) = ContextCompressor::split_summary_and_current_task(
            "摘要\n\n[[CURRENT_TASK]]\n  ".to_string(),
        );
        assert_eq!(summary, "摘要");
        assert!(task.is_none());
    }

    /// find_split_point：始终压缩到末尾，不再按 User 轮次保留。
    #[test]
    fn find_split_point_compresses_everything_after_summary_up_to() {
        let compressor = ContextCompressor::new(0);
        let messages = vec![
            user("U1"),
            assistant("A1"),
            user("U2"),
            assistant("A2"),
            user("U3"),
            assistant("A3"),
        ];
        // 无论消息多少，split_point 都应指向末尾（全压缩）。
        assert_eq!(compressor.find_split_point(&messages), 6);
    }

    /// find_split_point：工具堆积场景（单 User + 大量 Tool）也能正确返回末尾。
    /// 这是原 keep_recent_turns 策略失效的场景。
    #[test]
    fn find_split_point_handles_tool_accumulation() {
        let compressor = ContextCompressor::new(0);
        let messages = vec![
            user("单次提问"),
            assistant("调用工具1"),
            user("(tool_result)"),
            assistant("调用工具2"),
            user("(tool_result)"),
            assistant("调用工具3"),
            user("(tool_result)"),
        ];
        // 旧策略会返回 0（noop），新策略返回末尾，触发压缩。
        assert_eq!(compressor.find_split_point(&messages), 7);
    }

    /// build_compressed_resume_message：构造正确的合成消息。
    #[test]
    fn build_resume_message_marks_compressed_resume_phase() {
        use crate::session::{ContentBlock, MessagePhase};

        let msg = build_compressed_resume_message("用户问X，已得结果Y");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.phase, MessagePhase::CompressedResume);
        assert!(!msg.model_excluded); // 必须发给模型
        assert_eq!(msg.content.len(), 1);
        // 内容应为 ModelInstruction（前端 text_content 取不到，不渲染为可见文本）
        let text = match &msg.content[0] {
            ContentBlock::ModelInstruction { text } => text.clone(),
            other => panic!("期望 ModelInstruction，实际 {other:?}"),
        };
        assert!(text.contains("用户问X，已得结果Y"));
        // text_content 对 ModelInstruction 返回空（前端据此隐藏）
        assert_eq!(msg.text_content(), "");
    }

    /// compression_output_budget：按 context_limit 的 5%（1/20）如实计算，无下限。
    #[test]
    fn compression_output_budget_scales_with_context_limit() {
        // context_limit=0（未知）保守取 50k
        assert_eq!(
            ContextCompressor::new(0).compression_output_budget(),
            50_000
        );
        // 1M → 52428（5%）
        assert_eq!(
            ContextCompressor::new(1_048_576).compression_output_budget(),
            52_428
        );
        // 200k（实际使用下限）→ 10k（5%）
        assert_eq!(
            ContextCompressor::new(204_800).compression_output_budget(),
            10_240
        );
        // 8k（不在建议范围）→ 409，如实给出小预算，不人为抬高
        assert_eq!(
            ContextCompressor::new(8_192).compression_output_budget(),
            409
        );
    }
}
