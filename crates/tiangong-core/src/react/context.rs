//! ReAct 循环上下文管理：总结阶段请求、强制回复、上下文压缩

use std::sync::mpsc::Sender as StdSender;

use crate::context::organizer::ContextOrganizer;
use crate::model::{ModelClient, ModelRequest, SingleProviderClient, TokenUsage};
use crate::prompt::SystemPromptConfig;
use crate::runtime::{RuntimeEngine, use_stream_mode};
use crate::session::{Message, MessagePhase, MessageRole, Session, now_text};
use crate::stream_throttle::{StreamTextKind, ThrottledStreamSink};
use tiangong_types::StreamEvent;

/// 从 RuntimeEngine 配置构建 SystemPromptConfig 并重建 session 的 system prompt
pub(crate) fn rebuild_system_prompt(session: &mut Session, engine: &RuntimeEngine) {
    let plugin_sections = engine.collect_plugin_prompt_sections();
    let config = SystemPromptConfig::from_configs(
        engine.models_config(),
        engine.agent_config(),
        &session.id,
    )
    .with_plugin_sections(plugin_sections);
    session.rebuild_system_prompt(&config);
}

pub(crate) fn compression_threshold_tokens(context_limit: usize) -> usize {
    ContextOrganizer::new(context_limit)
        .with_threshold(0.95)
        .token_threshold()
}

pub(crate) fn emit_token_usage(
    stream_tx: &StdSender<StreamEvent>,
    usage: &TokenUsage,
    current_tokens: Option<usize>,
    context_limit: usize,
    source: impl Into<String>,
    agent_id: Option<&str>,
) {
    if usage.total_tokens == 0 {
        return;
    }
    let source = source.into();
    // 按 source（阶段）记录 KV cache 命中率，便于实测 ReAct/Summary 各阶段命中情况。
    // hit_ratio = hit / (hit + miss)，仅当 DeepSeek 返回了 cache 字段时记录。
    if let (Some(hit), Some(miss)) = (
        usage.prompt_cache_hit_tokens,
        usage.prompt_cache_miss_tokens,
    ) {
        let total = hit + miss;
        let ratio = if total > 0 {
            hit as f64 / total as f64
        } else {
            0.0
        };
        tracing::info!(
            source = %source,
            agent_id = ?agent_id,
            prompt_tokens = usage.prompt_tokens,
            completion_tokens = usage.completion_tokens,
            prompt_cache_hit_tokens = hit,
            prompt_cache_miss_tokens = miss,
            cache_hit_ratio = format!("{:.2}%", ratio * 100.0),
            "kv cache 命中统计",
        );
    }
    let _ = stream_tx.send(StreamEvent::TokenUsage {
        usage: usage.clone(),
        current_tokens,
        compression_threshold_tokens: Some(compression_threshold_tokens(context_limit)),
        context_limit_tokens: Some(context_limit),
        source,
        agent_id: agent_id.map(|s| s.to_string()),
    });
}

pub(crate) fn maybe_update_context_summary(
    session: &mut Session,
    engine: &RuntimeEngine,
    observed_usage: &TokenUsage,
    stream_tx: &StdSender<StreamEvent>,
) {
    let organizer = ContextOrganizer::new(engine.context_limit)
        .with_threshold(0.95)
        .with_keep_recent_turns(6);
    let observed_tokens = observed_total_tokens(observed_usage);
    if !organizer.needs_compression(observed_tokens) {
        return;
    }
    let total_messages = session.messages.len();
    let _ = stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: session.summary_up_to,
        total_messages,
    });
    match organizer.maybe_update_summary_with_usage(session, engine.client(), observed_tokens) {
        Ok(update) if update.compressed => {
            // 重置 current_tokens，压缩后上下文已大幅缩减
            session.current_tokens = 0;
            let remaining = session.messages.len().saturating_sub(session.summary_up_to);
            // 估算压缩后的 token 数（基于剩余消息比例）
            let estimated_tokens = (observed_tokens as f64
                * (remaining as f64 / total_messages.max(1) as f64))
                as usize;
            session.current_tokens = estimated_tokens;
            // 压缩后重建 system prompt（摘要已更新）
            rebuild_system_prompt(session, engine);
            emit_token_usage(
                stream_tx,
                &update.usage,
                Some(estimated_tokens),
                engine.context_limit,
                "context_summary",
                None,
            );
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: tiangong_types::stream::ContextCompressAction::Auto,
                summary_up_to: session.summary_up_to,
                remaining_messages: remaining,
            });
            session.persist_to_disk();
            tracing::info!(
                session_id = %session.id,
                observed_tokens,
                observed_prompt_tokens = observed_usage.prompt_tokens,
                observed_completion_tokens = observed_usage.completion_tokens,
                threshold_tokens = organizer.token_threshold(),
                summary_up_to = session.summary_up_to,
                "上下文与本轮输出达到压缩阈值，已更新早期对话摘要"
            );
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(
                session_id = %session.id,
                error = %err,
                "上下文压缩失败，继续使用原始上下文"
            );
        }
    }
}

pub(crate) fn observed_total_tokens(usage: &TokenUsage) -> usize {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.prompt_tokens.saturating_add(usage.completion_tokens)
    }
}

pub(crate) fn select_client_for_request<'a>(
    engine: &'a RuntimeEngine,
    _req: &ModelRequest,
) -> &'a SingleProviderClient {
    engine.client()
}

/// 总结阶段的判断指令（作为运行时上下文注入，不常驻 system prompt）。
///
/// 由主模型在总结阶段判断任务完成度：完成则给最终回复；需要用户提供信息则提问；
/// 仍有遗漏且本 Agent 能继续通过工具推进时，输出 [NEED_MORE_WORK] 触发重入 Loop。
pub(crate) const SUMMARY_PHASE_PROMPT: &str = "\
你当前处于总结阶段。请基于以上所有工作，给出最终回复。\n\
\n\
判断逻辑：\n\
1. 如果用户请求的所有操作都已执行并得到结果，请总结结果给出最终回复。\n\
2. 如果需要用户提供额外信息、凭据、授权、选择或确认才能继续，请直接向用户提问。\n\
3. 如果有关键步骤遗漏未执行、且你确实可以通过工具继续推进，请在回复开头输出 [NEED_MORE_WORK]，\n\
   然后简要说明还需要做什么。系统将重新进入工具执行阶段。\n\
\n\
注意：不要重复执行工具调用。不要重复已有内容。如果只是给用户后续建议，不要使用 [NEED_MORE_WORK]。";

/// 构建总结阶段的 LLM 请求。
///
/// 将 `SUMMARY_PHASE_PROMPT` 作为运行时上下文追加到对话末尾，不携带 tools，
/// 使用主模型 client，由主模型自行判断任务完成度并输出最终回复。
pub(crate) fn request_for_summary_phase(session: &Session) -> ModelRequest {
    let mut context = session.context();
    context.push(
        Message::new(
            MessageRole::System,
            format!("<runtime_context>\n{SUMMARY_PHASE_PROMPT}\n</runtime_context>"),
        )
        .with_phase(MessagePhase::Normal),
    );
    ModelRequest {
        session_title: session.title.clone(),
        user_input: String::new(),
        context,
        thinking: Some(crate::model::ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
        include_media: false,
    }
}

/// 强制最终回复的触发原因。
#[derive(Debug, Clone, Copy)]
pub(crate) enum ForceFinalReason {
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

/// 将错误持久化到 session，作为 LLM 请求失败时的诊断痕迹。
///
/// 前端 `StreamEvent::Error` 会在 commands.rs 写入 `[错误]` 系统消息，但存在
/// 时序丢失风险（execute_turn return 后转发线程可能未及时处理）。本函数在
/// engine 侧直接写入 session 并持久化，确保事后重载会话仍可看到失败原因。
pub(crate) fn persist_error(session: &mut Session, message: impl Into<String>) {
    let mut msg = Message::new(MessageRole::System, format!("[错误] {}", message.into()));
    msg.tool_name = Some("react_loop_error".to_string());
    session.messages.push(msg);
    session.persist_to_disk();
}

/// 超限时强制最终回复
pub(crate) fn force_final_response(
    session: &mut Session,
    engine: &RuntimeEngine,
    stream_tx: &StdSender<StreamEvent>,
    reason: ForceFinalReason,
) {
    // 确保 system prompt 已构建
    if session.system_prompt_message.is_none() {
        rebuild_system_prompt(session, engine);
    }
    // 注入提示消息到 session
    session.messages.push(Message {
        id: scru128::new().to_string(),
        role: MessageRole::Tool,
        content: vec![crate::session::ContentBlock::text(format!(
            "<system-reminder>\n{}\n</system-reminder>",
            reason.prompt()
        ))],
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        media: Vec::new(),
        media_migrated: true,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: Some("force_final_response".to_string()),
        tool_result_is_error: false,
        compact: false,
        phase: MessagePhase::Normal,
        created_at: now_text(),
    });

    let req = ModelRequest {
        session_title: session.title.clone(),
        user_input: String::new(),
        context: session.context(),
        thinking: Some(crate::model::ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
        include_media: false,
    };

    let pending_msg_id = scru128::new().to_string();

    let resp = if use_stream_mode() {
        let sink = ThrottledStreamSink::with_text_kind(
            pending_msg_id.clone(),
            stream_tx.clone(),
            StreamTextKind::Summary,
        );
        let response_result = select_client_for_request(engine, &req)
            .complete_stream_with_callback(&req, |delta| {
                sink.push_chunk(delta);
            });
        sink.finish();
        match response_result {
            Ok(r) => r,
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                persist_error(session, format!("force_final_response（流式）失败：{err}"));
                return;
            }
        }
    } else {
        let msg_id_non_stream = pending_msg_id.clone();
        match select_client_for_request(engine, &req).complete(&req) {
            Ok(r) => {
                if !r.text.is_empty() {
                    let _ = stream_tx.send(StreamEvent::SummaryText {
                        message_id: msg_id_non_stream,
                        content: r.text.clone(),
                    });
                }
                if !r.reasoning_content.is_empty() {
                    let _ = stream_tx.send(StreamEvent::Reasoning {
                        message_id: pending_msg_id.clone(),
                        content: r.reasoning_content.clone(),
                    });
                }
                r
            }
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                persist_error(
                    session,
                    format!("force_final_response（非流式）失败：{err}"),
                );
                return;
            }
        }
    };

    session.append_message_with_id(
        pending_msg_id,
        MessageRole::Assistant,
        resp.text,
        resp.reasoning_content,
    );
    if let Some(message) = session.messages.last_mut() {
        message.phase = MessagePhase::Summary;
        message.reasoning_signature = resp.reasoning_signature.clone();
    }
    emit_token_usage(
        stream_tx,
        &resp.usage,
        Some(resp.usage.prompt_tokens.max(session.current_tokens)),
        engine.context_limit,
        "force_final_response",
        None,
    );
    let _ = stream_tx.send(StreamEvent::Done {
        usage: Some(resp.usage.clone()),
    });
    session.persist_to_disk();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_total_tokens_prefers_provider_total() {
        let usage = TokenUsage {
            prompt_tokens: 900,
            completion_tokens: 120,
            total_tokens: 1100,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };

        assert_eq!(observed_total_tokens(&usage), 1100);
    }

    #[test]
    fn observed_total_tokens_falls_back_to_prompt_plus_completion() {
        let usage = TokenUsage {
            prompt_tokens: 900,
            completion_tokens: 120,
            total_tokens: 0,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };

        assert_eq!(observed_total_tokens(&usage), 1020);
    }
}
