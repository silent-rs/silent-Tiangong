use anyhow::Result;

use crate::model::{ModelRequest, TokenUsage};
use crate::session::{Message, MessagePhase, MessageRole, Session};
use crate::turn_context::TurnContext;

/// 判断消息是否为「可压缩的有效消息」。
///
/// 以下三类对压缩无价值，一律排除：
/// - `model_excluded`：已被显式排除（如被作废的续接消息）；
/// - `System`：system prompt 由调用方单独注入，不参与折叠；
/// - `CompressedResume`：前端不可见的合成续接消息，一次性使用，
///   不应计入 `messages_count`，也不应混入压缩请求污染摘要。
fn is_compressible(message: &Message) -> bool {
    !message.model_excluded
        && message.role != MessageRole::System
        && message.phase != MessagePhase::CompressedResume
}

/// 上下文压缩器
///
/// 持有 `&mut TurnContext`，压缩 `summary_up_to` 之后的所有消息（全压缩策略），
/// 早期历史细节由摘要和 `CompressedResume` 合成消息承接。
///
/// `compress()` 消耗自身，完成后对 `ctx` 的可变借用自动结束，可直接继续原流程。
/// 是否需要压缩由调用方判断（本类型不做阈值检查）。
///
/// 压缩阶段不可取消：压缩期间收到的命令保留在队列，压缩正常完成并保存结果。
/// 取消当前轮次时由调用方扫描作废未消费的 `CompressedResume` 续接消息。
pub struct ContextCompressor<'a> {
    ctx: &'a mut TurnContext,
}

#[derive(Debug, Clone, Default)]
pub struct CompressionUpdate {
    pub compressed: bool,
    pub usage: TokenUsage,
    /// 压缩时模型附带的「当前任务状态」，用于构造 `CompressedResume` 合成消息。
    pub current_task: Option<String>,
}

impl<'a> ContextCompressor<'a> {
    pub fn new(ctx: &'a mut TurnContext) -> Self {
        Self { ctx }
    }

    /// 压缩 `summary_up_to` 之后的所有消息。
    ///
    /// `inject_resume` 为真时，若模型附带了「当前任务状态」，注入
    /// `CompressedResume` 续接消息。仅在 **turn 进行中的自动压缩** 时传 `true`——
    /// 续接消息用于避免重试请求失忆。**手动压缩** 发生在独立 task，没有进行中的
    /// turn，续接消息无任务可承接，传 `false` 跳过注入。
    ///
    /// 完成后 `ctx` 已包含：新摘要、推进的 `summary_up_to`、压缩边界标记、
    /// （`inject_resume` 为真且有任务时）注入的 `CompressedResume` 续接消息、
    /// 累计的压缩用量、重建的 system prompt、已落盘的会话。返回
    /// [`CompressionUpdate`] 供调用方做界面反馈与用量估算。
    ///
    /// 摘要可能为空（模型输出空间不足等）。压缩主目的是释放上下文空间，
    /// 故仍推进边界；历史记忆由续接消息和未来的会话检索承接。
    ///
    /// # 错误
    ///
    /// 以下情况属于调用方错误，直接返回 `Err`（而非静默 noop）：
    /// - `summary_up_to` 之后没有消息（边界已在末尾，无可压缩内容）；
    /// - 待压缩范围内没有有效消息（全部被 `model_excluded` 或为 System）。
    ///
    /// 调用方应在到达阈值前确保有可压缩内容，到达本函数时无内容通常意味着
    /// 阈值/计数逻辑与实际消息流不一致。
    pub async fn compress(self, inject_resume: bool) -> Result<CompressionUpdate> {
        let split_point = self.ctx.session.messages.len();
        if split_point <= self.ctx.session.summary_up_to {
            anyhow::bail!(
                "无可压缩消息：summary_up_to({}) 已到达末尾({})，\
                 阈值/计数逻辑可能与实际消息流不一致",
                self.ctx.session.summary_up_to,
                split_point,
            );
        }
        let messages_count = self.ctx.session.messages[self.ctx.session.summary_up_to..split_point]
            .iter()
            .filter(|m| is_compressible(m))
            .count();
        if messages_count == 0 {
            anyhow::bail!(
                "待压缩范围 [{}..{}] 内无有效消息（全部被 model_excluded、为 System、\
                 或为前端不可见的 CompressedResume），阈值/计数逻辑可能与实际消息流不一致",
                self.ctx.session.summary_up_to,
                split_point,
            );
        }

        let (summary, current_task, usage) = self.fold_summary(split_point).await?;

        tracing::info!(
            old_summary_up_to = self.ctx.session.summary_up_to,
            new_summary_up_to = split_point,
            messages_count,
            summary_len = summary.len(),
            has_current_task = current_task.is_some(),
            inject_resume,
            "滚动摘要已更新"
        );

        let session = &mut self.ctx.session;
        session.context_summary = Some(summary);
        session.summary_up_to = split_point;
        mark_compact_boundary(&mut session.messages, split_point);

        // 仅在 turn 进行中的自动压缩注入续接消息，避免重试请求失忆。
        // 手动压缩（inject_resume=false）跳过：无进行中 turn，续接消息无承接对象。
        if inject_resume && let Some(task) = current_task.as_deref() {
            let resume = build_compressed_resume_message(task);
            let insert_at = session.summary_up_to.min(session.messages.len());
            session.messages.insert(insert_at, resume);
        }

        self.ctx.session.token_usage.accumulate(&usage);
        crate::react::context::rebuild_system_prompt(self.ctx);
        if let Err(error) = self.ctx.session.try_persist_to_disk() {
            tracing::warn!(%error, session_id = %self.ctx.session.id, "上下文压缩落盘失败");
        }

        Ok(CompressionUpdate {
            compressed: true,
            usage,
            current_task,
        })
    }

    /// 折叠摘要：构造请求、调用模型、解析输出。
    async fn fold_summary(
        &self,
        split_point: usize,
    ) -> Result<(String, Option<String>, TokenUsage)> {
        let req = Self::summary_request(&self.ctx.session, split_point)
            .with_max_output_tokens(self.compression_output_budget());
        let resp = self.ctx.client.complete_async(&req).await?;
        let (summary, current_task) = Self::split_summary_and_current_task(resp.text);
        Ok((summary, current_task, resp.usage))
    }

    /// 构造压缩请求：原对话延续模式。
    ///
    /// 压缩请求是原对话的延续——先让模型读完整段待压缩对话，末尾追加压缩指令
    /// 作为最后一条 User，头部注入 session 的 system prompt。不能 pop 队尾
    /// Assistant，否则会丢失待压缩内容。
    fn summary_request(session: &Session, split_point: usize) -> ModelRequest {
        let split_point = split_point.min(session.messages.len());
        let start = session.summary_up_to.min(split_point);
        let mut context = Vec::with_capacity((split_point - start) + 2);
        if let Some(system) = session.system_prompt_message.as_ref() {
            context.push(system.clone());
        }
        context.extend(
            session.messages[start..split_point]
                .iter()
                .filter(|m| is_compressible(m))
                .cloned(),
        );
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

    /// 压缩指令：拼接已有摘要供模型增量更新，并要求末尾附上「当前任务状态」。
    ///
    /// 模型输出格式约定为：
    /// ```text
    /// <摘要正文>
    ///
    /// [[CURRENT_TASK]]
    /// <当前任务状态>
    /// ```
    /// 解析时按此分隔符切分。
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

    /// 压缩请求的输出 token 预算。
    ///
    /// # 建议使用 context ≥ 200k 的模型
    ///
    /// 压缩摘要 + 「当前任务状态」段落的输出量较大，过小的输出预算会让模型
    /// 截断输出，导致 `[[CURRENT_TASK]]` 段落缺失、压缩后 turn 内重试失忆。
    /// 因此**建议选用上下文窗口 ≥ 200k 的模型**：按下方 5% 预算计算，200k
    /// 可得 10k+ tokens 的输出空间，足以承载完整摘要与任务状态。
    ///
    /// # 预算推导
    ///
    /// 压缩常在 prompt 达到 context_limit 的 95%（预防性压缩阈值）时触发，
    /// 此时 provider 允许的 completion ≈ context_limit 的 5%。声明的 max_tokens
    /// 不应超过这个剩余——部分 provider 严格校验 `prompt + max_tokens ≤ limit`，
    /// 声明过大（如写死 32k）会直接报错。
    ///
    /// 按 context_limit 的 5%（1/20）如实计算，不设下限：
    /// - 实际使用的 LLM ≥ 200k（建议范围），5% = 10k+，写摘要充足；
    /// - 小上下文（<200k，不在建议范围）如实给出小预算，不人为抬高下限
    ///   导致声明超过实际剩余而报错；此时摘要可能被截断。
    fn compression_output_budget(&self) -> u32 {
        const RATIO: u32 = 20; // 1/20 ≈ 5%，对齐预防性压缩阈值
        if self.ctx.context_limit == 0 {
            // context_limit 未知时保守取 50k（对齐 1M 上下文的 5%）。
            return 50_000;
        }
        (self.ctx.context_limit as u32) / RATIO
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
/// 内容由模型在压缩时附带的「当前任务状态」提供。
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
    use crate::session::Session;

    fn user(text: &str) -> Message {
        Message::new(MessageRole::User, text)
    }

    fn assistant(text: &str) -> Message {
        Message::new(MessageRole::Assistant, text)
    }

    /// summary_request：单轮 body=[User, Assistant]，压缩指令 append 到末尾，
    /// 原对话完整保留。
    #[test]
    fn summary_request_appends_instruction_to_full_body() {
        let mut session = Session::new("test");
        session.messages = vec![user("你好"), assistant("你好，有什么可以帮你？")];

        let req = ContextCompressor::summary_request(&session, session.messages.len());

        // System 缺省，context = [User(你好), Assistant(回复), User(压缩指令)]
        assert_eq!(req.context.len(), 3);
        assert_eq!(req.context[0].role, MessageRole::User);
        assert_eq!(req.context[0].text_content(), "你好");
        assert_eq!(req.context[1].role, MessageRole::Assistant);
        assert_eq!(req.context[1].text_content(), "你好，有什么可以帮你？");
        assert_eq!(req.context[2].role, MessageRole::User);
        assert!(req.context[2].text_content().contains("压缩为简洁摘要"));
    }

    /// summary_request：system_prompt_message 存在时作为首条注入。
    #[test]
    fn summary_request_prepends_system_prompt_when_present() {
        let mut session = Session::new("test");
        session.system_prompt_message = Some(Message::new(MessageRole::System, "你是天工助手。"));
        session.messages = vec![user("问题"), assistant("回答")];

        let req = ContextCompressor::summary_request(&session, session.messages.len());

        assert_eq!(req.context.len(), 4);
        assert_eq!(req.context[0].role, MessageRole::System);
        assert_eq!(req.context[0].text_content(), "你是天工助手。");
        assert_eq!(req.context[3].role, MessageRole::User);
        assert!(req.context[3].text_content().contains("压缩为简洁摘要"));
    }

    /// summary_request：已有摘要拼接到压缩指令末尾，供模型增量更新。
    #[test]
    fn summary_request_appends_existing_summary_to_instruction() {
        let mut session = Session::new("test");
        session.context_summary = Some("旧摘要内容".to_string());
        session.messages = vec![user("问题"), assistant("回答")];

        let req = ContextCompressor::summary_request(&session, session.messages.len());

        let instruction = req.context.last().unwrap().text_content();
        assert!(instruction.contains("压缩为简洁摘要"));
        assert!(instruction.contains("[已有摘要]"));
        assert!(instruction.contains("旧摘要内容"));
    }

    /// split_summary_and_current_task：正常分隔符切分。
    #[test]
    fn split_extracts_summary_and_current_task() {
        let text = "这是摘要正文。\n\n[[CURRENT_TASK]]\n用户最近提问：X\n进行中：工具Y";
        let (summary, task) = ContextCompressor::split_summary_and_current_task(text.to_string());
        assert_eq!(summary, "这是摘要正文。");
        assert_eq!(task.as_deref(), Some("用户最近提问：X\n进行中：工具Y"));
    }

    /// split_summary_and_current_task：无分隔符时整体视为摘要。
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

    /// is_compressible：前端不可见的 CompressedResume 不算有效消息，
    /// 即使它的 role 是 User 且 model_excluded=false。
    #[test]
    fn is_compressible_excludes_compressed_resume() {
        use crate::session::MessagePhase;

        let normal_user = user("你好");
        assert!(is_compressible(&normal_user));

        let mut resume = user("续接任务状态");
        resume.phase = MessagePhase::CompressedResume;
        // CompressedResume 默认 model_excluded=false（需发给模型），但不可压缩
        assert!(!resume.model_excluded);
        assert!(!is_compressible(&resume));
    }

    /// summary_request：前端不可见的 CompressedResume 不混入压缩请求，
    /// 避免污染摘要。
    #[test]
    fn summary_request_excludes_compressed_resume_messages() {
        use crate::session::MessagePhase;

        let mut session = Session::new("test");
        let mut resume = user("续接任务状态");
        resume.phase = MessagePhase::CompressedResume;
        session.messages = vec![user("你好"), assistant("回复"), resume];

        let req = ContextCompressor::summary_request(&session, session.messages.len());

        // context = [User(你好), Assistant(回复), User(压缩指令)]，
        // CompressedResume 被排除，不进入压缩请求。
        assert_eq!(req.context.len(), 3);
        assert_eq!(req.context[0].text_content(), "你好");
        assert_eq!(req.context[1].text_content(), "回复");
        assert!(req.context[2].text_content().contains("压缩为简洁摘要"));
        assert!(
            !req.context
                .iter()
                .any(|m| m.phase == MessagePhase::CompressedResume),
            "压缩请求不应包含 CompressedResume 消息"
        );
    }
}
