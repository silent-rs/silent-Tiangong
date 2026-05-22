//! ReAct 循环上下文管理：memory 注入、强制回复、上下文压缩

use std::sync::mpsc::Sender as StdSender;

use crate::context::organizer::ContextOrganizer;
use crate::model::{ModelClient, ModelRequest, SingleProviderClient, TokenUsage};
use crate::prompt::PromptAssembler;
use crate::runtime::{RuntimeEngine, use_stream_mode};
use crate::session::{Message, MessageRole, Session, now_text};
use crate::stream_throttle::ThrottledStreamSink;
use tiangong_types::StreamEvent;

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
    let _ = stream_tx.send(StreamEvent::TokenUsage {
        usage: usage.clone(),
        current_tokens,
        compression_threshold_tokens: Some(compression_threshold_tokens(context_limit)),
        context_limit_tokens: Some(context_limit),
        source: source.into(),
        agent_id: agent_id.map(|s| s.to_string()),
    });
}

pub(crate) fn maybe_update_context_summary(
    session: &mut Session,
    engine: &RuntimeEngine,
    observed_prompt_tokens: usize,
    stream_tx: &StdSender<StreamEvent>,
) {
    let organizer = ContextOrganizer::new(engine.context_limit)
        .with_threshold(0.95)
        .with_keep_recent_turns(6);
    if !organizer.needs_compression(observed_prompt_tokens) {
        return;
    }
    let total_messages = session.messages.len();
    let _ = stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: session.summary_up_to,
        total_messages,
    });
    match organizer.maybe_update_summary_with_usage(
        session,
        engine.client(),
        observed_prompt_tokens,
    ) {
        Ok(update) if update.compressed => {
            // 重置 current_tokens，压缩后上下文已大幅缩减
            session.current_tokens = 0;
            let remaining = session.messages.len().saturating_sub(session.summary_up_to);
            // 估算压缩后的 token 数（基于剩余消息比例）
            let estimated_tokens = (observed_prompt_tokens as f64
                * (remaining as f64 / total_messages.max(1) as f64))
                as usize;
            session.current_tokens = estimated_tokens;
            emit_token_usage(
                stream_tx,
                &update.usage,
                Some(estimated_tokens),
                engine.context_limit,
                "context_summary",
                None,
            );
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: "自动压缩".to_string(),
                summary_up_to: session.summary_up_to,
                remaining_messages: remaining,
            });
            session.persist_to_disk();
            tracing::info!(
                session_id = %session.id,
                observed_prompt_tokens,
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

pub(crate) fn select_client_for_request<'a>(
    engine: &'a RuntimeEngine,
    _req: &ModelRequest,
) -> &'a SingleProviderClient {
    engine.client()
}

const COMPLETION_CHECK_SYSTEM_PROMPT: &str = "\
你是一个任务完成度判断器。你需要判断 AI 助手是否已经充分完成了用户的请求。

判断标准（偏向判定为已完成）：
- 助手是否给出了实质性的回复内容（代码改动说明、执行结果、分析结论等）
- 如果助手回复了具体的操作结果（如编译通过、文件已修改、命令已执行），应判定为已完成
- 只有当回复明显空洞（如只有\"好的\"\"让我来\"等过渡语）或用户请求明显需要操作但助手未执行任何工具时，才判定为未完成

回复规则：
- 已完成：COMPLETE
- 未完成：INCOMPLETE
- 只回复一个词，不要添加任何其他文字";

/// 使用 lite 模型判断当前回复是否真正完成了任务。
/// 返回 `true` 表示任务已完成，`false` 表示需要继续 react 循环。
/// 如果 lite 模型调用失败，降级返回 `true`（直接结束，不影响原有流程）。
pub(crate) async fn check_completion_with_lite_model(
    engine: &RuntimeEngine,
    user_input: &str,
    assistant_response: &str,
) -> bool {
    if assistant_response.trim().is_empty() {
        return false;
    }

    let lite = engine.lite_client().clone();
    let system_prompt = COMPLETION_CHECK_SYSTEM_PROMPT.to_string();
    let user_prompt = format_completion_check_prompt(user_input, assistant_response);

    let result = tokio::task::spawn_blocking(move || {
        lite.complete_lite_with_system(&system_prompt, &user_prompt)
    })
    .await;

    match result {
        Ok(Ok(response_text)) => parse_completion_response(&response_text),
        _ => {
            tracing::warn!("lite 模型完成度检测失败，降级为直接结束");
            true
        }
    }
}

fn format_completion_check_prompt(user_input: &str, assistant_response: &str) -> String {
    format!(
        "用户原始请求：\n{user_input}\n\n\
         AI 助手的最新回复：\n{assistant_response}\n\n\
         请判断：AI 助手是否已经充分完成了用户的请求？"
    )
}

fn parse_completion_response(response: &str) -> bool {
    let trimmed = response.trim().to_uppercase();
    trimmed.contains("COMPLETE") && !trimmed.contains("INCOMPLETE")
}

pub(crate) fn loop_context_with_memory(
    loop_context: &[Message],
    memory_context: Option<&str>,
) -> Vec<Message> {
    let mut messages = loop_context.to_vec();
    let Some(ctx) = memory_context.map(str::trim).filter(|ctx| !ctx.is_empty()) else {
        return messages;
    };

    messages.insert(
        0,
        Message {
            id: scru128::new().to_string(),
            role: MessageRole::Tool,
            content: format!(
                "<memory-recall>\n{ctx}\n</memory-recall>\n\
                请基于以上 recall_memory 检索结果继续完成用户原始目标；不要再次调用 recall_memory，除非用户提出新的历史查询。"
            ),
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: Some("recall_memory".to_string()),
            tool_result_is_error: false,
            compact: false,
            created_at: now_text(),
        },
    );
    messages
}

/// 超限时强制最终回复
pub(crate) fn force_final_response(
    session: &mut Session,
    loop_context: &[Message],
    engine: &RuntimeEngine,
    stream_tx: &StdSender<StreamEvent>,
) {
    let mut final_context = loop_context.to_vec();
    final_context.push(Message {
        id: scru128::new().to_string(),
        role: MessageRole::Tool,
        content:
            "<system-reminder>\n请基于以上所有工具执行结果，直接给出最终回复。\n</system-reminder>"
                .to_string(),
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        media: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: Some("force_final_response".to_string()),
        tool_result_is_error: false,
        compact: false,
        created_at: now_text(),
    });

    let assembler = PromptAssembler::new(engine.context_limit);
    let assembled = assembler.assemble(
        session,
        "",
        Vec::new(),
        engine.models_config(),
        engine.agent_config(),
        &final_context,
    );

    let req = ModelRequest {
        session_title: session.title.clone(),
        user_input: assembled.user_input.clone(),
        context: assembled.build_messages(),
        thinking: Some(crate::model::ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
        include_media: false,
    };

    let pending_msg_id = scru128::new().to_string();

    let resp = if use_stream_mode() {
        let sink = ThrottledStreamSink::new(pending_msg_id.clone(), stream_tx.clone());
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
                return;
            }
        }
    } else {
        let msg_id_non_stream = pending_msg_id.clone();
        match select_client_for_request(engine, &req).complete(&req) {
            Ok(r) => {
                if !r.text.is_empty() {
                    let _ = stream_tx.send(StreamEvent::Delta {
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
                return;
            }
        }
    };

    session.append_message_with_id(
        pending_msg_id,
        MessageRole::Assistant,
        resp.text,
        String::new(),
    );
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
}
