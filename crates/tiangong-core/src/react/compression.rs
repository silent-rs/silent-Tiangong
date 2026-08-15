//! ReAct 上下文压缩任务的启动、取消、结果提交和通知。

use anyhow::{Result, bail};
use tokio::sync::mpsc as tokio_mpsc;

use crate::context::compressor::{
    CompressionError, CompressionUpdate, ContextCompressor, is_compressible, mark_compact_boundary,
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

/// 压缩种类：三种场景的全部差异都在这里参数化（启动参数、用量归属、通知文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompressionKind {
    /// 请求前压力压缩：含当前任务续接，用量计入 turn 累计。
    Auto { observed_tokens: usize },
    /// 上下文溢出强制压缩：从强制分割点开始，用量计入 turn 累计。
    Forced,
    /// 手动压缩：不含当前任务续接，用量计入 session 并显式落盘。
    Manual { observed_tokens: usize },
}

/// 压缩会话的中断原因（`run` 返回；取消类命令已在内部分流）。
pub(crate) enum CompressionInterrupt {
    Command(Command),
    Closed,
}

/// `run` 的命令策略。
pub(crate) enum CommandPolicy<'a> {
    /// 任何命令都取消压缩并上抛，由调用方（主循环）统一处理（Auto/Forced）。
    Relay,
    /// 终止类命令（Cancel/Shutdown）取消并上抛；新输入（引导消息/工具注入）
    /// 经 `defer_input` 转 Inbox 排队后**继续等待压缩完成**（压缩是独立维护
    /// 活动，不因新输入让路，ALR-104：压缩完成后由同一 driver 继续排队的
    /// 输入）；配置类就地消化。转排队失败时取消压缩并上抛（不丢）。
    ConsumeLocally {
        defer_input: &'a (dyn Fn(Command) -> Result<(), Command> + Send + Sync),
    },
}

/// 一次统一的上下文压缩会话：启动 → 等待（命令可中断）→ 提交。
///
/// 三种压缩场景（请求前压力、溢出强制、手动）共用同一份等待与提交逻辑，
/// 差异全部由 [`CompressionKind`] 表达；压缩不再是 Agent 顶层阶段（ALR-303）。
pub(crate) struct ContextCompression {
    task: CompressionTask,
    kind: CompressionKind,
}

impl ContextCompression {
    /// 发起请求前压力压缩。
    pub(super) fn auto(
        ctx: &TurnContext,
        organizer: &ContextOrganizer,
        observed_tokens: usize,
    ) -> Self {
        Self::start(
            ctx,
            organizer,
            observed_tokens,
            CompressionKind::Auto { observed_tokens },
        )
    }

    /// 发起强制压缩（上下文溢出恢复）。
    pub(super) fn forced(ctx: &TurnContext, organizer: &ContextOrganizer) -> Self {
        Self::start(ctx, organizer, 0, CompressionKind::Forced)
    }

    /// 发起手动压缩。
    fn manual(ctx: &TurnContext, organizer: &ContextOrganizer, observed_tokens: usize) -> Self {
        Self::start(
            ctx,
            organizer,
            observed_tokens,
            CompressionKind::Manual { observed_tokens },
        )
    }

    /// 统一启动：三种压缩共用同一分割点（保留最近一次完整交互，折叠更早
    /// 历史），产出只是 session 的一次调整（摘要 + 边界推进），不注入任何
    /// 合成消息——当前任务由 Loop 保留的锚点用户消息承载。
    fn start(
        ctx: &TurnContext,
        organizer: &ContextOrganizer,
        observed_tokens: usize,
        kind: CompressionKind,
    ) -> Self {
        notify_started(ctx);
        Self {
            task: start_task(
                ContextCompressor::new(ctx.session.clone(), ctx.client.clone()),
                organizer,
                observed_tokens,
                compression_split_point(&ctx.session),
            ),
            kind,
        }
    }

    /// 取消压缩任务并通知取消（压缩被命令中断时）。
    pub(crate) async fn cancel(&mut self, ctx: &TurnContext) {
        cancel_task(std::mem::replace(&mut self.task, noop_task()), ctx).await;
    }

    /// 压缩与命令双路等待：命令优先（biased）；到达时取消压缩并上抛。
    pub(crate) async fn run(
        &mut self,
        ctx: &mut TurnContext,
        cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
        policy: CommandPolicy<'_>,
    ) -> std::result::Result<CompressionResult, CompressionInterrupt> {
        loop {
            tokio::select! {
                biased;
                command = cmd_rx.recv() => {
                    let command = match command {
                        Some(command) => command,
                        None => {
                            self.cancel(ctx).await;
                            return Err(CompressionInterrupt::Closed);
                        }
                    };
                    match policy {
                        CommandPolicy::Relay => {
                            self.cancel(ctx).await;
                            return Err(CompressionInterrupt::Command(command));
                        }
                        CommandPolicy::ConsumeLocally { defer_input } => match command {
                            Command::Cancel | Command::Shutdown => {
                                self.cancel(ctx).await;
                                return Err(CompressionInterrupt::Command(command));
                            }
                            // 压缩独立于用户意图：新输入转 Inbox 排队，压缩继续；
                            // 转排队失败（会话关闭）才取消并上抛（不丢）。
                            Command::InjectUserMessage { .. } | Command::InjectTool { .. } => {
                                if let Err(command) = defer_input(command) {
                                    self.cancel(ctx).await;
                                    return Err(CompressionInterrupt::Command(command));
                                }
                            }
                            // 配置类命令就地消化（保持原手动压缩行为）。
                            Command::SetReasoningEffort(effort) => {
                                ctx.agent_config.reasoning_effort = effort.clone();
                                ctx.session.reasoning_effort = Some(effort);
                            }
                            Command::SetTrustMode(mode) => {
                                ctx.trust_mode = mode;
                                ctx.session.trust_mode = mode;
                            }
                            _ => {}
                        },
                    }
                }
                task_result = &mut self.task => {
                    return Ok(resolve_task_result(task_result));
                }
            }
        }
    }

    /// 提交压缩结果：应用摘要、按种类累计用量并通知（ALR-307）。
    ///
    /// `result` 来自 `run` 的完成返回。Auto/Forced 的用量计入 `turn_usage`
    /// （turn 累计）；Manual 计入 session 并显式落盘。
    pub(crate) fn complete(
        self,
        ctx: &mut TurnContext,
        result: CompressionResult,
        turn_usage: Option<&mut TokenUsage>,
    ) {
        let Self { kind, task } = self;
        drop(task);
        match kind {
            CompressionKind::Forced => {
                let observed_tokens = ctx.context_limit;
                complete_with_turn_usage(
                    ctx,
                    turn_usage.expect("Forced 压缩必须提供 turn 用量"),
                    observed_tokens,
                    result,
                );
            }
            CompressionKind::Auto { observed_tokens } => {
                complete_with_turn_usage(
                    ctx,
                    turn_usage.expect("Auto 压缩必须提供 turn 用量"),
                    observed_tokens,
                    result,
                );
            }
            CompressionKind::Manual { .. } => complete_manual(ctx, result),
        }
    }
}

/// 取消后的占位任务（run 已返回，占位仅为保持结构可丢弃，永不完成）。
fn noop_task() -> CompressionTask {
    tokio::spawn(async { std::future::pending::<CompressionResult>().await })
}

/// Auto/Forced 的统一提交：应用、累计 turn 用量并通知。
fn complete_with_turn_usage(
    ctx: &mut TurnContext,
    turn_usage: &mut TokenUsage,
    observed_tokens: usize,
    result: CompressionResult,
) {
    match result {
        Ok(update) => {
            turn_usage.accumulate(&update.usage);
            match apply_compression(ctx, &update, false) {
                Ok(current_tokens) => {
                    notify_auto_success(ctx, &update, current_tokens, observed_tokens);
                }
                Err(error) => {
                    notify_auto_failure(ctx, &update.usage, &error);
                }
            }
        }
        Err(error) => {
            turn_usage.accumulate(&error.usage);
            notify_auto_failure(ctx, &error.usage, &error);
        }
    }
}

/// 在独立 turn task 中执行手动压缩。
pub(crate) async fn run_manual_context_compression(
    mut ctx: TurnContext,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    defer_input: &(dyn Fn(Command) -> Result<(), Command> + Send + Sync),
) -> Option<CompressionInterrupt> {
    let observed_tokens = ctx.session.current_tokens;
    let organizer = ContextOrganizer::new(ctx.context_limit);

    let compressor = ContextCompressor::new(ctx.session.clone(), ctx.client.clone());
    if !compressor.has_pending_messages() {
        notify_result(&ctx, ContextCompressAction::Noop);
        return None;
    }
    let mut compression = ContextCompression::manual(&ctx, &organizer, observed_tokens);
    match compression
        .run(
            &mut ctx,
            &mut cmd_rx,
            CommandPolicy::ConsumeLocally { defer_input },
        )
        .await
    {
        Ok(result) => {
            compression.complete(&mut ctx, result, None);
            None
        }
        // 中断类命令已取消压缩，未应用任何结果；引导消息/工具注入原样上抛，
        // 由调用方转入 Inbox 排队（不丢失，ALR-102/202）。
        Err(interrupt) => Some(interrupt),
    }
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

/// 压缩分割点：保留最近一次完整交互（最新可见消息及其所属工具批次），
/// 之前的可压缩历史（含锚点用户消息本身）折叠为摘要。
///
/// 锚点被折叠时由 `apply_compression` 注入 LLM 可见、用户不可见的锚点消息
///（用户最近请求原文），满足 Provider「首条非 System 消息必须是 User」约束。
fn compression_split_point(session: &Session) -> Option<usize> {
    let start = session.summary_up_to.min(session.messages.len());
    let last_visible = (start..session.messages.len()).rev().find(|&index| {
        let message = &session.messages[index];
        !message.model_excluded && message.role != MessageRole::System
    })?;
    let last_message = &session.messages[last_visible];
    let recent_start = if last_message.role == MessageRole::Tool {
        last_message
            .tool_call_id
            .as_deref()
            .and_then(|tool_call_id| {
                (start..last_visible).rev().find(|&index| {
                    let message = &session.messages[index];
                    message.role == MessageRole::Assistant
                        && message
                            .tool_calls
                            .iter()
                            .any(|call| call.id == tool_call_id)
                })
            })
            .unwrap_or(last_visible)
    } else {
        last_visible
    };

    session.messages[start..recent_start]
        .iter()
        .any(is_compressible)
        .then_some(recent_start)
}

/// 构造 LLM 可见、用户不可见的锚点消息：`User` 角色（满足 Provider 首条
/// 约束）+ `CompressedResume` 阶段（前端不展示/搜索/编辑）+ `ModelInstruction`
/// 内容块（进入模型请求，不属于用户可见文本）。内容是被折叠锚点的原文，
/// 由程序直接复制，不经过 LLM 提取。
fn build_anchor_resume_message(anchor_text: &str) -> Message {
    let mut message = Message::new(MessageRole::User, "");
    message.content = vec![ContentBlock::model_instruction(format!(
        "用户当前请求（压缩后原文保留）：\n\n{anchor_text}"
    ))];
    message.phase = MessagePhase::CompressedResume;
    message
}

/// 被折叠区间内最后一条可见用户消息的文本（锚点原文）。
fn folded_anchor_text(session: &Session, folded_end: usize) -> Option<String> {
    let start = session.summary_up_to.min(folded_end);
    (start..folded_end)
        .rev()
        .find(|index| {
            let message = &session.messages[*index];
            !message.model_excluded && message.role == MessageRole::User
        })
        .map(|index| session.messages[index].text_content())
        .filter(|text| !text.trim().is_empty())
}

fn start_task(
    compressor: ContextCompressor,
    organizer: &ContextOrganizer,
    observed_tokens: usize,
    split_point: Option<usize>,
) -> CompressionTask {
    let output_budget = organizer.compression_output_budget(observed_tokens);
    tokio::spawn(async move {
        let split_point = split_point.ok_or_else(|| {
            CompressionError::new("上下文已超限，但没有可与最近交互分离的较早历史，无法压缩")
        })?;
        let output_budget = output_budget.ok_or_else(|| {
            CompressionError::new("上下文剩余空间不足 2048 tokens，无法生成有效摘要")
        })?;
        compressor.compress(split_point, output_budget).await
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
    // 保留区不以 User 开头（锚点被折叠）时，注入 LLM 可见、用户不可见的
    // 锚点消息承载用户最近请求原文（程序复制，非 LLM 提取）。
    let needs_anchor = candidate
        .messages
        .get(update.summary_up_to)
        .is_none_or(|message| message.role != MessageRole::User || message.model_excluded);
    if needs_anchor
        && let Some(anchor_text) = folded_anchor_text(&ctx.session, update.summary_up_to)
    {
        candidate.messages.insert(
            update.summary_up_to,
            build_anchor_resume_message(&anchor_text),
        );
    }
    candidate.context_summary = Some(update.summary.clone());
    candidate.summary_up_to = update.summary_up_to;
    mark_compact_boundary(&mut candidate.messages, candidate.summary_up_to);
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
    notify_usage(
        ctx,
        &update.usage,
        Some(current_tokens),
        "manual_context_compress",
    );
    notify_result(ctx, ContextCompressAction::Compress);
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
    use crate::agent_config::AgentConfig;
    use crate::model::SingleProviderClient;
    use crate::observe::Observer;
    use crate::permission::TrustMode;
    use crate::session::Message;
    use crate::session::MessagePhase;
    use crate::session::MessageToolCall;
    use tiangong_llm::{ModelEndpoint, ProviderProtocol};

    fn test_context(mut session: Session) -> (TurnContext, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("创建临时目录失败");
        session.bind_storage_root(root.path());
        let (stream_tx, _) = std::sync::mpsc::channel();
        let client = SingleProviderClient::new(ModelEndpoint {
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            timeout_ms: 1_000,
            options: serde_json::Value::Object(serde_json::Map::new()),
        });
        let ctx = TurnContext::builder()
            .client(client)
            .session(session)
            .stream_tx(stream_tx)
            .plugins(Vec::new())
            .context_limit(200_000)
            .agent_config(AgentConfig::default())
            .trust_mode(TrustMode::FullTrust)
            .observer(Observer::new(std::env::temp_dir()))
            .tool_overrides(Default::default())
            .tools(Vec::new())
            .build();
        (ctx, root)
    }

    fn update_for(
        session: &Session,
        previous_summary_up_to: usize,
        summary: &str,
        summary_up_to: usize,
    ) -> CompressionUpdate {
        CompressionUpdate {
            summary: summary.to_string(),
            usage: TokenUsage::default(),
            previous_summary_up_to,
            summary_up_to,
            boundary_message_id: session.messages[summary_up_to - 1].id.clone(),
        }
    }

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
    fn split_point_keeps_the_latest_complete_tool_batch() {
        let mut session = Session::new("test");
        session.append_message(MessageRole::User, "较早问题");
        session.append_message(MessageRole::Assistant, "较早回答");
        session.append_message(MessageRole::User, "锚点问题");

        let mut assistant = Message::new(MessageRole::Assistant, "");
        assistant.tool_calls = vec![MessageToolCall {
            id: "latest-call".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "latest.txt"}),
        }];
        session.messages.push(assistant);
        session.messages.push(Message::tool_result(
            "latest-call",
            "read_file",
            "最近工具结果",
            false,
        ));
        let mut hidden = Message::new(MessageRole::System, "仅前端可见状态");
        hidden.model_excluded = true;
        session.messages.push(hidden);

        // 锚点用户消息允许被折叠（由锚点消息机制续接）：保留最近工具批次。
        assert_eq!(compression_split_point(&session), Some(3));
    }

    #[test]
    fn split_point_requires_history_before_the_latest_interaction() {
        let mut session = Session::new("test");
        session.append_message(MessageRole::User, "当前问题");
        assert_eq!(compression_split_point(&session), None);

        session
            .messages
            .insert(0, Message::new(MessageRole::User, "较早问题"));
        assert_eq!(compression_split_point(&session), Some(1));
    }

    /// 压缩只是 session 的一次调整：摘要推进边界，不注入任何合成消息；
    /// 最近交互保留在模型上下文中（当前任务由 Loop 的锚点用户消息承载）。
    #[test]
    fn compression_folds_history_and_keeps_recent_interaction_in_context() {
        let mut session = Session::new("test");
        for round in ["第一轮", "第二轮", "第三轮"] {
            session.append_message(MessageRole::User, format!("{round}问题"));
            session.append_message(MessageRole::Assistant, format!("{round}回答"));
        }
        let (mut ctx, _root) = test_context(session);

        // 模拟压缩分割点：保留第三轮（最近交互），折叠前两轮。
        let update = update_for(&ctx.session, 0, "前两轮摘要", 4);
        apply_compression(&mut ctx, &update, false).expect("压缩应成功");

        assert_eq!(ctx.session.context_summary.as_deref(), Some("前两轮摘要"));
        assert_eq!(ctx.session.summary_up_to, 4);
        let context = ctx.session.context();
        assert_eq!(context[0].role, MessageRole::System);
        assert_eq!(context[1].text_content(), "第三轮问题");
        assert_eq!(context[2].text_content(), "第三轮回答");
        assert!(
            ctx.session
                .messages
                .iter()
                .all(|message| message.phase != MessagePhase::CompressedResume),
            "压缩不注入合成续接消息"
        );
    }
}
