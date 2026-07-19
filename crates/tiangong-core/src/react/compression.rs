//! ReAct 上下文压缩任务的启动、取消、结果提交和通知。

use anyhow::{Result, bail};
use tokio::sync::mpsc as tokio_mpsc;

use crate::context::compressor::{
    CompressionError, CompressionUpdate, ContextCompressor, mark_compact_boundary,
};
use crate::context::organizer::ContextOrganizer;
use crate::core::command::Command;
use crate::model::TokenUsage;
use crate::session::{ContentBlock, Message, MessagePhase, MessageRole, Session};
use crate::turn_context::TurnContext;
use tiangong_types::{StreamEvent, stream::ContextCompressAction};

use super::cancel::abort_and_join;
use super::context::{emit_token_usage, rebuild_system_prompt_for_session};

pub(super) type CompressionResult = std::result::Result<CompressionUpdate, CompressionError>;
type CompressionTask = tokio::task::JoinHandle<CompressionResult>;

pub(super) struct ActiveCompression<C> {
    task: CompressionTask,
    observed_tokens: usize,
    continuation: C,
}

impl<C> ActiveCompression<C> {
    pub(super) fn start(
        ctx: &TurnContext,
        organizer: &ContextOrganizer,
        observed_tokens: usize,
        continuation: C,
        force_target_budget: bool,
    ) -> Self {
        notify_started(ctx);
        let budget_tokens = if force_target_budget {
            0
        } else {
            observed_tokens
        };
        Self {
            task: start_task(
                ContextCompressor::new(ctx.session.clone(), ctx.client.clone()),
                organizer,
                budget_tokens,
                true,
            ),
            observed_tokens,
            continuation,
        }
    }

    pub(super) async fn wait(&mut self) -> CompressionResult {
        resolve_task_result((&mut self.task).await)
    }

    pub(super) async fn cancel(self, ctx: &TurnContext) {
        cancel_task(self.task, ctx).await;
    }

    pub(super) fn complete(
        self,
        ctx: &mut TurnContext,
        accumulated_usage: &mut TokenUsage,
        result: CompressionResult,
    ) -> (C, Option<Message>) {
        let Self {
            observed_tokens,
            continuation,
            ..
        } = self;
        let resume = match result {
            Ok(update) => {
                accumulated_usage.accumulate(&update.usage);
                match apply_compression(ctx, &update, false) {
                    Ok(current_tokens) => {
                        notify_auto_success(ctx, &update, current_tokens, observed_tokens);
                        update
                            .current_task
                            .as_deref()
                            .map(build_compression_resume_message)
                    }
                    Err(error) => {
                        notify_auto_failure(ctx, &update.usage, &error);
                        None
                    }
                }
            }
            Err(error) => {
                accumulated_usage.accumulate(&error.usage);
                notify_auto_failure(ctx, &error.usage, &error);
                None
            }
        };
        (continuation, resume)
    }
}

/// 在独立 turn task 中执行手动压缩。
pub(crate) async fn run_manual_context_compression(
    mut ctx: TurnContext,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
) {
    let observed_tokens = ctx.session.current_tokens;
    let organizer = ContextOrganizer::new(ctx.context_limit);
    notify_started(&ctx);

    let compressor = ContextCompressor::new(ctx.session.clone(), ctx.client.clone());
    if !compressor.has_pending_messages() {
        notify_result(&ctx, ContextCompressAction::Noop);
        return;
    }
    let mut task = start_task(compressor, &organizer, observed_tokens, false);

    let wait_for_cancel = async {
        loop {
            match cmd_rx.recv().await {
                Some(Command::Cancel | Command::Shutdown) | None => return,
                Some(_) => {}
            }
        }
    };
    tokio::pin!(wait_for_cancel);
    let result = tokio::select! {
        biased;
        _ = &mut wait_for_cancel => {
            cancel_task(task, &ctx).await;
            return;
        }
        result = &mut task => resolve_task_result(result)
    };
    complete_manual(&mut ctx, result);
}

pub(crate) fn notify_cleared(stream_tx: &std::sync::mpsc::Sender<StreamEvent>, session: &Session) {
    notify_session_result(stream_tx, session, ContextCompressAction::Clear);
}

pub(super) fn observed_total_tokens(usage: &TokenUsage) -> usize {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.prompt_tokens.saturating_add(usage.completion_tokens)
    }
}

pub(super) fn build_compression_resume_message(current_task: &str) -> Message {
    let mut message = Message::new(MessageRole::User, "");
    message.content = vec![ContentBlock::model_instruction(format!(
        "以下是上下文压缩后的当前任务状态，请据此继续执行，不要重新询问用户：\n\n{current_task}"
    ))];
    message.phase = MessagePhase::Normal;
    message
}

fn start_task(
    compressor: ContextCompressor,
    organizer: &ContextOrganizer,
    observed_tokens: usize,
    include_current_task: bool,
) -> CompressionTask {
    let output_budget = organizer.compression_output_budget(observed_tokens);
    tokio::spawn(async move {
        let output_budget = output_budget.ok_or_else(|| {
            CompressionError::new("上下文剩余空间不足 2048 tokens，无法生成有效摘要")
        })?;
        compressor
            .compress(output_budget, include_current_task)
            .await
    })
}

fn resolve_task_result(
    result: std::result::Result<CompressionResult, tokio::task::JoinError>,
) -> CompressionResult {
    result.unwrap_or_else(|error| Err(CompressionError::new(error.to_string())))
}

async fn cancel_task(task: CompressionTask, ctx: &TurnContext) {
    abort_and_join(task).await;
    notify_result(ctx, ContextCompressAction::Cancelled);
}

fn complete_manual(ctx: &mut TurnContext, result: CompressionResult) {
    match result {
        Ok(update) => match apply_compression(ctx, &update, true) {
            Ok(current_tokens) => notify_manual_success(ctx, &update, current_tokens),
            Err(error) => {
                ctx.session.token_usage.accumulate(&update.usage);
                ctx.session.persist_to_disk();
                notify_manual_failure(ctx, &update.usage, &error);
            }
        },
        Err(error) => {
            ctx.session.token_usage.accumulate(&error.usage);
            ctx.session.persist_to_disk();
            notify_manual_failure(ctx, &error.usage, &error);
        }
    }
}

fn apply_compression(
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
    rebuild_system_prompt_for_session(&mut candidate, &ctx.plugins);
    candidate
        .try_persist_to_disk()
        .map_err(anyhow::Error::msg)?;
    ctx.session = candidate;
    Ok(current_tokens)
}

fn notify_started(ctx: &TurnContext) {
    let _ = ctx.stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: ctx.session.summary_up_to,
        total_messages: ctx.session.messages.len(),
    });
}

fn notify_auto_success(
    ctx: &TurnContext,
    update: &CompressionUpdate,
    current_tokens: usize,
    observed_tokens: usize,
) {
    notify_result(ctx, ContextCompressAction::Auto);
    notify_usage(ctx, &update.usage, Some(current_tokens), "context_summary");
    tracing::info!(
        session_id = %ctx.session.id,
        observed_tokens,
        old_summary_up_to = update.previous_summary_up_to,
        summary_up_to = ctx.session.summary_up_to,
        total_messages = update.summary_up_to,
        "上下文摘要已更新"
    );
}

fn notify_auto_failure(ctx: &TurnContext, usage: &TokenUsage, error: &dyn std::fmt::Display) {
    notify_usage(ctx, usage, None, "context_summary_failed");
    tracing::warn!(
        session_id = %ctx.session.id,
        error = %error,
        "上下文压缩失败，保留原始上下文"
    );
    notify_result(ctx, ContextCompressAction::Failed);
}

fn notify_manual_success(ctx: &TurnContext, update: &CompressionUpdate, current_tokens: usize) {
    notify_result(ctx, ContextCompressAction::Compress);
    notify_usage(
        ctx,
        &update.usage,
        Some(current_tokens),
        "manual_context_compress",
    );
    tracing::info!(
        session_id = %ctx.session.id,
        summary_up_to = ctx.session.summary_up_to,
        "手动上下文摘要已更新"
    );
}

fn notify_manual_failure(ctx: &TurnContext, usage: &TokenUsage, error: &dyn std::fmt::Display) {
    notify_usage(ctx, usage, None, "manual_context_compress_failed");
    tracing::warn!(
        session_id = %ctx.session.id,
        error = %error,
        "手动上下文压缩失败，继续使用原始上下文"
    );
    let _ = ctx.stream_tx.send(StreamEvent::AgentNotification {
        agent_id: "system".to_string(),
        agent_label: "系统".to_string(),
        content: format!("手动压缩上下文失败：{error}"),
        level: "error".to_string(),
    });
    notify_result(ctx, ContextCompressAction::Failed);
}

fn notify_usage(
    ctx: &TurnContext,
    usage: &TokenUsage,
    current_tokens: Option<usize>,
    source: &'static str,
) {
    emit_token_usage(
        &ctx.stream_tx,
        usage,
        current_tokens,
        ctx.context_limit,
        source,
        None,
    );
}

fn notify_result(ctx: &TurnContext, action: ContextCompressAction) {
    notify_session_result(&ctx.stream_tx, &ctx.session, action);
}

fn notify_session_result(
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    session: &Session,
    action: ContextCompressAction,
) {
    let _ = stream_tx.send(StreamEvent::ContextCompressed {
        action,
        summary_up_to: session.summary_up_to,
        remaining_messages: session.messages.len().saturating_sub(session.summary_up_to),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_tokens_prefers_provider_total() {
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
    fn observed_tokens_fall_back_to_prompt_plus_completion() {
        let usage = TokenUsage {
            prompt_tokens: 900,
            completion_tokens: 120,
            total_tokens: 0,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        };

        assert_eq!(observed_total_tokens(&usage), 1020);
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
