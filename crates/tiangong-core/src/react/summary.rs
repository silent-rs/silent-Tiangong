//! 最终化（Finalization）模块。
//!
//! 本模块只保留总结请求构建、结果解析和消息提交；模型流与取消统一由
//! `execute_turn` 的事件循环驱动。

use crate::model::ModelRequest;
use crate::react::context::{emit_token_usage, rebuild_system_prompt};
use crate::react::message::upsert_assistant_text_message;
use crate::session::{Message, MessagePhase, MessageRole, Session, now_text};
use crate::turn_context::TurnContext;

/// 把 session 中最后一条 `phase=React` 的 assistant 消息提升为最终回复（`phase=Summary`）。
///
/// 用于总结阶段判定为「空正文 Done」时：LLM 认为上一轮 ReAct 已有完整可用的答复，
/// 无需新内容。此时直接复用上一轮回复作为最终回复，避免落盘空消息造成重复展示。
/// 找不到符合条件的消息时不做任何改动（兜底）。
pub(super) fn promote_last_react_message_to_summary(session: &mut Session) -> Option<String> {
    for message in session.messages.iter_mut().rev() {
        if message.role == MessageRole::Assistant
            && message.phase == crate::session::MessagePhase::React
        {
            message.phase = crate::session::MessagePhase::Summary;
            return Some(message.id.clone());
        }
    }
    tracing::warn!("空正文 Done 但未找到可提升的 React 消息，保持现状");
    None
}

/// 总结阶段对任务完成度的判定结果。
///
/// 由 [`parse_summary_phase_output`] 从模型回复的标记解析得到。语义上：
/// - `Done`：任务完成（含普通最终回复与 `[DONE]`），本轮结束
/// - `AskUser`：需要用户提供信息（`[ASK_USER]`），视作本轮结束
/// - `NeedMoreWork`：未完成但可继续（`[NEED_MORE_WORK]`），重入 ReAct Loop
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SummaryDecision {
    Done(String),
    AskUser(String),
    NeedMoreWork(String),
}

impl SummaryDecision {
    /// 取判定附带的文本正文（去标记后的回复内容）。
    pub(super) fn payload(&self) -> &str {
        match self {
            SummaryDecision::Done(s)
            | SummaryDecision::AskUser(s)
            | SummaryDecision::NeedMoreWork(s) => s,
        }
    }
}

/// 解析总结阶段回复，得到完成度判定。
///
/// - 首行 `[NEED_MORE_WORK]` → [`SummaryDecision::NeedMoreWork`]（去标记后的正文为下一步说明）
/// - 首行 `[ASK_USER]` → [`SummaryDecision::AskUser`]（去标记后的正文为向用户的提问）
/// - 首行 `[DONE]` → [`SummaryDecision::Done`]（去标记后的正文为最终回复）
/// - 无标记 → 视为完成，[`SummaryDecision::Done`]（原文）
pub(super) fn parse_summary_phase_output(text: &str) -> SummaryDecision {
    let trimmed = text.trim();
    if let Some(rest) = strip_summary_marker(trimmed, "[NEED_MORE_WORK]") {
        return SummaryDecision::NeedMoreWork(rest.trim().to_string());
    }
    if let Some(rest) = strip_summary_marker(trimmed, "[ASK_USER]") {
        return SummaryDecision::AskUser(rest.trim().to_string());
    }
    if let Some(rest) = strip_summary_marker(trimmed, "[DONE]") {
        return SummaryDecision::Done(rest.trim().to_string());
    }
    SummaryDecision::Done(trimmed.to_string())
}

fn strip_summary_marker<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let text = text.trim_start();
    let prefix = text.get(..marker.len())?;
    if !prefix.eq_ignore_ascii_case(marker) {
        return None;
    }
    Some(
        text[marker.len()..]
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | '-')),
    )
}

pub(super) fn persist_partial_summary(
    ctx: &mut crate::turn_context::TurnContext,
    message_id: &str,
    text: &str,
    reasoning: &str,
) {
    if text.trim().is_empty() && reasoning.trim().is_empty() {
        return;
    }
    upsert_assistant_text_message(
        &mut ctx.session,
        message_id,
        text,
        reasoning,
        crate::session::MessagePhase::Summary,
    );
    if let Err(error) = ctx.session.try_persist_to_disk() {
        tracing::warn!(%error, "持久化部分总结响应失败");
    }
    crate::react::message::emit_session_message_upsert(ctx, message_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_marker_parses_to_done() {
        assert_eq!(
            parse_summary_phase_output("[DONE]\n已完成。变更包括 A 和 B。"),
            SummaryDecision::Done("已完成。变更包括 A 和 B。".to_string())
        );
    }

    #[test]
    fn done_marker_case_insensitive_and_strips_separators() {
        // 大小写不敏感；标记后可跟冒号/中文冒号/连字符/空白
        assert_eq!(
            parse_summary_phase_output("[done]：已完成。"),
            SummaryDecision::Done("已完成。".to_string())
        );
    }

    #[test]
    fn ask_user_marker_parses_to_ask_user() {
        assert_eq!(
            parse_summary_phase_output("[ASK_USER] 请提供 API 凭据。"),
            SummaryDecision::AskUser("请提供 API 凭据。".to_string())
        );
    }

    #[test]
    fn need_more_work_marker_parses_to_need_more_work() {
        assert_eq!(
            parse_summary_phase_output("[NEED_MORE_WORK] 还需运行测试并修复失败用例。"),
            SummaryDecision::NeedMoreWork("还需运行测试并修复失败用例。".to_string())
        );
    }

    #[test]
    fn no_marker_defaults_to_done() {
        // 无标记视为完成，保留原文。
        assert_eq!(
            parse_summary_phase_output("  任务已全部完成。  "),
            SummaryDecision::Done("任务已全部完成。".to_string())
        );
    }

    #[test]
    fn empty_text_defaults_to_done_empty() {
        assert_eq!(
            parse_summary_phase_output("   "),
            SummaryDecision::Done(String::new())
        );
    }

    #[test]
    fn payload_returns_inner_text() {
        assert_eq!(SummaryDecision::Done("a".into()).payload(), "a");
        assert_eq!(SummaryDecision::AskUser("b".into()).payload(), "b");
        assert_eq!(SummaryDecision::NeedMoreWork("c".into()).payload(), "c");
    }

    #[test]
    fn promote_last_react_message_promotes_the_last_react_assistant() {
        // 构造一个含多条消息的 session：用户消息 + React 过程回复 + 工具结果。
        let mut session = Session::new("test");
        session.append_message(MessageRole::User, "帮我创建定时任务");
        session.append_message_with_id(
            "m1".to_string(),
            MessageRole::Assistant,
            "已创建定时提醒：每天 9 点叫你起床。",
            String::new(),
        );
        // 把这条 assistant 消息标记为 React（模拟 ReAct 过程回复）
        if let Some(m) = session.messages.last_mut() {
            m.phase = MessagePhase::React;
        }

        promote_last_react_message_to_summary(&mut session);

        // 最后一条 assistant 消息应被提升为 Summary
        let promoted = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .expect("应存在 assistant 消息");
        assert_eq!(promoted.phase, MessagePhase::Summary);
    }

    #[test]
    fn promote_last_react_message_promotes_only_the_latest() {
        // 多条 React 消息时，只提升最后一条。
        let mut session = Session::new("test");
        for i in 0..3 {
            session.append_message_with_id(
                format!("m{i}"),
                MessageRole::Assistant,
                format!("过程回复 {i}"),
                String::new(),
            );
            if let Some(m) = session.messages.last_mut() {
                m.phase = MessagePhase::React;
            }
        }

        promote_last_react_message_to_summary(&mut session);

        // 只有最后一条（"过程回复 2"）变为 Summary，其余仍为 React
        let react_count = session
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant && m.phase == MessagePhase::React)
            .count();
        let summary_count = session
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant && m.phase == MessagePhase::Summary)
            .count();
        assert_eq!(react_count, 2, "前两条应仍为 React");
        assert_eq!(summary_count, 1, "只有最后一条被提升为 Summary");
    }

    #[test]
    fn promote_last_react_message_noop_without_react_message() {
        // session 中没有 React assistant 消息时不做任何改动（兜底）。
        let mut session = Session::new("test");
        session.append_message(MessageRole::User, "你好");
        session.append_message_with_id(
            "m1".to_string(),
            MessageRole::Assistant,
            "你好，有什么可以帮你？",
            String::new(),
        );
        // 这条 assistant 是默认的 Normal，不是 React
        let before_phase = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .map(|m| m.phase)
            .unwrap();

        promote_last_react_message_to_summary(&mut session);

        let after_phase = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .map(|m| m.phase)
            .unwrap();
        assert_eq!(before_phase, after_phase, "无 React 消息时不应改动");
    }
}

/// 总结阶段的判断指令（作为运行时上下文注入，不常驻 system prompt）。
///
/// 总结阶段的核心职责是「完成度判断器 + 续作路由」，而非无条件重写最终答案：
/// 完成则结束、未完成且可继续则 NEED_MORE_WORK 回到 ReAct、需要用户输入则提问。
pub(super) const SUMMARY_PHASE_PROMPT: &str = "\
你当前处于总结阶段。你首先是一个「完成度判断器」，其次才是最终回复的作者。\n\
请先判断上一轮 ReAct 是否已经完成用户任务，再据此输出。\n\
\n\
判断与输出规则（请在回复首行用对应标记）：\n\
1. 若上一轮已经给出完整、可用的最终答复：只输出 [DONE]，不要带任何正文。\n\
   系统会自动复用上一轮回复作为最终回复，复述只会造成重复展示。\n\
2. 若任务已完成，但上一轮回复缺少面向用户的最终说明（例如只输出了工具结果、\n\
   没有总结性文字）：输出 [DONE]，并在新行给出最终回复正文。\n\
3. 若任务未完成、且你确实还能通过工具继续推进（例如只查了一半、改了没验证、\n\
   测试失败但可继续修、还有明确下一步）：输出 [NEED_MORE_WORK]，\n\
   然后简要说明还需要做什么。系统将重新进入工具执行阶段。\n\
4. 若需要用户提供信息（凭据、授权、选择、确认）才能继续：输出 [ASK_USER]，\n\
   然后提出问题。这视作本轮结束。\n\
\n\
输出原则：\n\
- 规则 1 是默认情况：只要上一轮已有可用的最终答复，就只输出 [DONE]，不要复述。\n\
- 仅当确实需要补充新的面向用户的说明时，才在标记后给出正文（规则 2/3/4）。\n\
- 本阶段不会执行任何工具调用。不要在回复中要求调用工具。\n\
- 如果只是给用户后续建议（而非确实还有未完成工作），不要使用 [NEED_MORE_WORK]。";

/// 构建总结阶段的 LLM 请求。
///
/// 将 `SUMMARY_PHASE_PROMPT` 作为运行时上下文追加到对话末尾。请求本身不携带 tools
/// 选择信息；`execute_turn` 启动请求时显式使用 `ToolChoice::None`。
pub(super) fn request_for_summary_phase(session: &Session) -> ModelRequest {
    build_text_finalization_request(
        session,
        &format!("<runtime_context>\n{SUMMARY_PHASE_PROMPT}\n</runtime_context>"),
    )
}

/// 构建一次「只产出文本最终回复」的 LLM 请求（共用请求体）。
///
/// 总结与强制终结仅在 runtime context 上不同；传空串时不追加 System 消息。
fn build_text_finalization_request(session: &Session, prompt: &str) -> ModelRequest {
    let mut context = session.context();
    if !prompt.is_empty() {
        context.push(
            Message::new(MessageRole::System, prompt.to_string()).with_phase(MessagePhase::Normal),
        );
    }
    ModelRequest {
        user_input: String::new(),
        context,
        thinking: Some(crate::model::ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
        max_output_tokens: None,
    }
}

/// 强制最终回复的触发原因。
#[derive(Debug, Clone, Copy)]
pub(super) enum ForceFinalReason {
    /// 总结阶段后重入 Loop 的次数已达上限。
    OuterLimit,
    /// 总结阶段 LLM 请求失败。
    SummaryError,
}

impl ForceFinalReason {
    fn prompt(self) -> &'static str {
        match self {
            Self::OuterLimit => {
                "任务已经过多轮迭代仍未完全完成。请基于以上所有工作给出最终回复。\n\
要求：\n\
1. 总结已完成的操作和结果。\n\
2. 如果有未完成的任务，说明原因和后续建议。\n\
3. 不要重复执行工具调用。\n\
4. 如果需要用户提供信息才能继续，请明确列出需要什么。"
            }
            Self::SummaryError => {
                "总结阶段执行失败。请基于以上所有工作，尽量给出最终回复。\n\
要求：\n\
1. 总结已完成的操作和结果。\n\
2. 如果有未完成的任务，说明原因和后续建议。\n\
3. 不要重复执行工具调用。\n\
4. 如果需要用户提供信息才能继续，请明确列出需要什么。"
            }
        }
    }
}

pub(super) fn build_force_final_request(
    ctx: &mut TurnContext,
    reason: ForceFinalReason,
) -> ModelRequest {
    if ctx.session.system_prompt_message.is_none() {
        rebuild_system_prompt(ctx);
    }
    ctx.session.messages.push(Message {
        id: scru128::new().to_string(),
        role: MessageRole::Tool,
        content: vec![crate::session::ContentBlock::text(format!(
            "<system-reminder>\n{}\n</system-reminder>",
            reason.prompt()
        ))],
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        elapsed_ms: None,
        turn_status: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: Some("force_final_response".to_string()),
        tool_result_is_error: false,
        compact: false,
        model_excluded: false,
        phase: MessagePhase::Normal,
        created_at: now_text(),
    });

    build_text_finalization_request(&ctx.session, "")
}

pub(super) fn commit_summary_message(
    ctx: &mut TurnContext,
    pending_msg_id: &str,
    response: &crate::model::ModelFunctionResponse,
    usage_source: &str,
) -> Result<(), String> {
    if response.text.trim().is_empty() && !response.invalid_tool_calls.is_empty() {
        let errors = response
            .invalid_tool_calls
            .iter()
            .take(3)
            .map(|call| format!("tool={} id={}：{}", call.name, call.id, call.reason))
            .collect::<Vec<_>>()
            .join("；");
        return Err(format!("最终回复为空，工具调用参数未通过校验：{errors}"));
    }

    ctx.session.append_message_with_id(
        pending_msg_id.to_string(),
        MessageRole::Assistant,
        response.text.clone(),
        response.reasoning_content.clone(),
    );
    if let Some(message) = ctx.session.messages.last_mut() {
        message.phase = MessagePhase::Summary;
        message.reasoning_signature = response.reasoning_signature.clone();
    }
    emit_token_usage(
        &ctx.stream_tx,
        &response.usage,
        Some(response.usage.prompt_tokens.max(ctx.session.current_tokens)),
        ctx.context_limit,
        usage_source,
        None,
    );
    ctx.session
        .try_persist_to_disk()
        .map_err(|error| format!("持久化最终回复失败：{error}"))?;
    crate::react::message::emit_session_message_upsert(ctx, pending_msg_id);
    Ok(())
}
