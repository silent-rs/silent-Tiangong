//! ReAct 循环上下文管理：system prompt 重建、token usage 上报、上下文压缩。
//!
//! 最终化相关逻辑（总结阶段、强制最终回复）已迁移到 `summary.rs`。

use std::sync::mpsc::Sender as StdSender;

use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::context::organizer::ContextOrganizer;
use crate::core::command::Command;
use crate::model::{ModelRequest, SingleProviderClient, TokenUsage};
use crate::prompt::SystemPromptConfig;
use crate::turn_context::TurnContext;
use tiangong_types::{StreamEvent, stream::ContextCompressAction};

/// 从本轮插件快照收集段落并重建 session 的 system prompt。
///
/// 产品身份 / 通用规则 / 自定义指令外围等文案由各插件经 `PromptSectionProvider`
/// 注入（产品基础文案见 `tiangong-plugin-prompt`），core 不再持有产品文案。
pub(crate) fn rebuild_system_prompt(ctx: &mut TurnContext) {
    let plugin_sections = ctx
        .plugins
        .iter()
        .flat_map(|plugin| plugin.prompt_sections())
        .collect();
    let config = SystemPromptConfig::from_plugin_sections(plugin_sections);
    ctx.session.rebuild_system_prompt(&config);
}

pub(crate) fn build_thinking_config(
    ctx: &TurnContext,
) -> (
    Option<crate::model::ThinkingConfig>,
    Option<crate::model::ReasoningEffort>,
    bool,
) {
    let effort = ctx.agent_config.reasoning_effort.trim().to_lowercase();
    match effort.as_str() {
        "none" | "" => (None, None, true),
        "low" => (
            Some(crate::model::ThinkingConfig {
                budget_tokens: 4096,
            }),
            Some(crate::model::ReasoningEffort::Low),
            false,
        ),
        "medium" => (
            Some(crate::model::ThinkingConfig {
                budget_tokens: 4096,
            }),
            Some(crate::model::ReasoningEffort::Medium),
            false,
        ),
        "high" => (
            Some(crate::model::ThinkingConfig {
                budget_tokens: 8192,
            }),
            Some(crate::model::ReasoningEffort::High),
            false,
        ),
        "max" => (
            Some(crate::model::ThinkingConfig {
                budget_tokens: 16384,
            }),
            Some(crate::model::ReasoningEffort::Max),
            false,
        ),
        _ => (
            Some(crate::model::ThinkingConfig {
                budget_tokens: 4096,
            }),
            Some(crate::model::ReasoningEffort::Medium),
            false,
        ),
    }
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

pub(crate) async fn maybe_update_context_summary(
    ctx: &mut TurnContext,
    observed_usage: &TokenUsage,
    action: ContextCompressAction,
    mut cancel_rx: oneshot::Receiver<()>,
) -> bool {
    let is_manual = action == ContextCompressAction::Compress;
    let organizer = ContextOrganizer::new(ctx.context_limit)
        .with_threshold(0.95)
        .with_keep_recent_turns(6);
    let observed_tokens = observed_total_tokens(observed_usage);
    if !organizer.needs_compression(observed_tokens) {
        if is_manual {
            let _ = ctx.stream_tx.send(StreamEvent::ContextCompressed {
                action: ContextCompressAction::Noop,
                summary_up_to: ctx.session.summary_up_to,
                remaining_messages: ctx
                    .session
                    .messages
                    .len()
                    .saturating_sub(ctx.session.summary_up_to),
            });
        }
        return false;
    }
    let total_messages = ctx.session.messages.len();
    let _ = ctx.stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: ctx.session.summary_up_to,
        total_messages,
    });
    let summary_up_to_before = ctx.session.summary_up_to;
    let remaining_before = total_messages.saturating_sub(summary_up_to_before);
    let update = {
        let update_future = organizer.maybe_update_summary_with_usage_async(
            &mut ctx.session,
            &ctx.client,
            observed_tokens,
        );
        tokio::pin!(update_future);
        tokio::select! {
            biased;
            _ = &mut cancel_rx => {
                if is_manual {
                    let _ = ctx.stream_tx.send(StreamEvent::ContextCompressed {
                        action: ContextCompressAction::Cancelled,
                        summary_up_to: summary_up_to_before,
                        remaining_messages: remaining_before,
                    });
                }
                return true;
            },
            update = &mut update_future => update,
        }
    };

    match update {
        Ok(update) if update.compressed => {
            ctx.session.current_tokens = 0;
            let remaining = ctx
                .session
                .messages
                .len()
                .saturating_sub(ctx.session.summary_up_to);
            let current_tokens = if is_manual {
                ctx.session.active_agent_current_tokens = 0;
                ctx.session.agent_current_tokens.clear();
                0
            } else {
                let estimated_tokens = (observed_tokens as f64
                    * (remaining as f64 / total_messages.max(1) as f64))
                    as usize;
                ctx.session.current_tokens = estimated_tokens;
                // 自动压缩后当前 turn 还会继续，必须立即刷新包含新摘要的提示。
                rebuild_system_prompt(ctx);
                estimated_tokens
            };
            ctx.session.token_usage.accumulate(&update.usage);
            if let Err(error) = ctx.session.try_persist_to_disk() {
                tracing::warn!(%error, session_id = %ctx.session.id, "上下文压缩落盘失败");
                if is_manual {
                    let _ = ctx.stream_tx.send(StreamEvent::ContextCompressed {
                        action: ContextCompressAction::Failed,
                        summary_up_to: ctx.session.summary_up_to,
                        remaining_messages: remaining,
                    });
                }
                return false;
            }
            emit_token_usage(
                &ctx.stream_tx,
                &update.usage,
                Some(current_tokens),
                ctx.context_limit,
                if is_manual {
                    "manual_context_compress"
                } else {
                    "context_summary"
                },
                None,
            );
            let _ = ctx.stream_tx.send(StreamEvent::ContextCompressed {
                action,
                summary_up_to: ctx.session.summary_up_to,
                remaining_messages: remaining,
            });
            tracing::info!(
                session_id = %ctx.session.id,
                observed_tokens,
                observed_prompt_tokens = observed_usage.prompt_tokens,
                observed_completion_tokens = observed_usage.completion_tokens,
                threshold_tokens = organizer.token_threshold(),
                summary_up_to = ctx.session.summary_up_to,
                "上下文与本轮输出达到压缩阈值，已更新早期对话摘要"
            );
        }
        Ok(_) => {
            if is_manual {
                let _ = ctx.stream_tx.send(StreamEvent::ContextCompressed {
                    action: ContextCompressAction::Noop,
                    summary_up_to: ctx.session.summary_up_to,
                    remaining_messages: ctx
                        .session
                        .messages
                        .len()
                        .saturating_sub(ctx.session.summary_up_to),
                });
            }
        }
        Err(err) => {
            tracing::warn!(
                session_id = %ctx.session.id,
                error = %err,
                "上下文压缩失败，继续使用原始上下文"
            );
            if is_manual {
                let _ = ctx.stream_tx.send(StreamEvent::AgentNotification {
                    agent_id: "system".to_string(),
                    agent_label: "系统".to_string(),
                    content: format!("手动压缩上下文失败：{err}"),
                    level: "error".to_string(),
                });
                let _ = ctx.stream_tx.send(StreamEvent::ContextCompressed {
                    action: ContextCompressAction::Failed,
                    summary_up_to: ctx.session.summary_up_to,
                    remaining_messages: ctx
                        .session
                        .messages
                        .len()
                        .saturating_sub(ctx.session.summary_up_to),
                });
            }
        }
    }

    false
}

pub(crate) fn observed_total_tokens(usage: &TokenUsage) -> usize {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.prompt_tokens.saturating_add(usage.completion_tokens)
    }
}

/// 在独立 turn task 中执行手动压缩，并把运行时命令转发给统一压缩流程。
pub(crate) async fn run_manual_context_compression(
    mut ctx: TurnContext,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
) {
    let forced_usage = TokenUsage {
        prompt_tokens: ctx.context_limit,
        completion_tokens: 0,
        total_tokens: ctx.context_limit,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };
    let stream_tx = ctx.stream_tx.clone();
    let context_limit = ctx.context_limit;
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let mut cancel_tx = Some(cancel_tx);
    let mut compression_future = Box::pin(maybe_update_context_summary(
        &mut ctx,
        &forced_usage,
        ContextCompressAction::Compress,
        cancel_rx,
    ));
    let mut reported_usage = TokenUsage::default();
    let mut pending_trust_mode = None;
    let mut deferred_tool_injections = Vec::new();
    let mut session_changed = false;

    loop {
        tokio::select! {
            biased;
            command = cmd_rx.recv() => match command {
                Some(Command::Cancel | Command::Shutdown) | None => {
                    cmd_rx.close();
                    if let Some(cancel_tx) = cancel_tx.take() {
                        let _ = cancel_tx.send(());
                    }
                    let _ = (&mut compression_future).await;
                    break;
                }
                Some(Command::Approval { .. }) => {}
                Some(Command::SetTrustMode(mode)) => {
                    pending_trust_mode = Some(mode);
                    session_changed = true;
                }
                Some(Command::InjectTool { tool_name, payload }) => {
                    deferred_tool_injections.push((tool_name, payload));
                }
                Some(Command::EmitStreamEvent(event)) => {
                    let _ = stream_tx.send(*event);
                }
                Some(Command::ReportUsage {
                    usage,
                    source,
                    emit_event,
                }) => {
                    reported_usage.accumulate(&usage);
                    session_changed = true;
                    if emit_event {
                        emit_token_usage(
                            &stream_tx,
                            &usage,
                            None,
                            context_limit,
                            source,
                            None,
                        );
                    }
                }
            },
            _ = &mut compression_future => break,
        }
    }
    drop(compression_future);

    ctx.session.token_usage.accumulate(&reported_usage);
    if let Some(mode) = pending_trust_mode {
        ctx.trust_mode = mode;
        ctx.session.trust_mode = mode;
        for plugin in &ctx.plugins {
            plugin.set_trust_mode(mode);
        }
    }
    let had_tool_injections = !deferred_tool_injections.is_empty();
    for (tool_name, payload) in deferred_tool_injections {
        crate::react::message::defer_tool_injection(&mut ctx, tool_name, payload);
    }
    if had_tool_injections {
        crate::react::message::flush_deferred_tool_injections(&mut ctx);
    }
    if session_changed && !had_tool_injections {
        let _ = ctx.session.try_persist_to_disk();
    }
}

pub(crate) fn select_client_for_request<'a>(
    ctx: &'a TurnContext,
    _req: &ModelRequest,
) -> &'a SingleProviderClient {
    ctx.client()
}

/// 将错误持久化到 session，作为 LLM 请求失败时的诊断痕迹。
///
/// 复用 `inject_tool_to_messages` 统一注入通道（合法的 assistant tool_call + tool result
/// 消息对），而非手工追加消息：
/// - 避免使用 `MessageRole::System`——它会被 `build_provider_messages` 的
///   `system_texts.clear()` 覆盖整个 system prompt，污染 prompt 并破坏 KV cache 前缀；
/// - 走标准注入通道保证 append-only、provider 序列化为合法 tool pair，cache 友好。
///
/// 去重边界：`inject_tool_to_messages` 仅对连续相同的 plugin_injection 结果去重，
/// 不会与前端 `StreamEvent::Error` 落盘的 `[错误]` System 消息跨格式去重——因此
/// 前端若也落盘，UI 上仍可能出现重复错误消息。engine 侧落盘作为前端时序丢失的
/// 兜底，确保会话重载后至少能看到失败原因。
pub(crate) fn persist_error(ctx: &mut TurnContext, message: impl Into<String>) {
    let message = message.into();
    let payload = serde_json::json!({
        "error": message,
        "instruction": "上一步执行失败，请基于已有结果继续或向用户说明原因。",
    });
    crate::react::message::inject_tool_to_messages(&mut ctx.session, "react_loop_error", &payload);
    ctx.session.persist_to_disk();
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
