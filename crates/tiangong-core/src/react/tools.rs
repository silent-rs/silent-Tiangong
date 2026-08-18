//! 工具执行流水线（ALR-301/302，任务 17）。
//!
//! 模型响应中的工具调用整批交给 [`execute_tool_batch`]：规范化与参数校验
//! （去重/无效调用上下文）→ 权限判断 → 必要时等待审批 → 有界并行执行 →
//! 顺序提交结果并更新 [`TaskContract`](super::contract::TaskContract) → 闭合
//! Provider 工具协议。审批是流水线内部的异步等待，与工具共享同一命令通道：
//! 取消/引导到达时批次收敛闭合后把命令交还执行驱动，Loop 不再拥有
//! `PreparingTools` / `WaitingTools` / `WaitingApproval` 顶层阶段。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::{Id as TaskId, JoinSet};

use crate::core::command::Command;
use crate::core::plugin::Plugin;
use crate::permission::TrustMode;
use crate::turn_context::TurnContext;
use tiangong_types::StreamEvent;

use super::command::Deferred;
use super::compression::observed_total_tokens;
use super::execute::{
    AgentLoopState, CompletedToolCall, ToolInjectionBuffer, ToolPreflightOutcome,
    append_failure_recovery_prompt, append_invalid_tool_calls_context, prepare_tool_call,
    record_completed_tool_call, record_parallel_duplicate_tool_call, set_runtime_trust_mode,
};
use super::helpers::record_plugin_usage;
use super::message::append_tool_result_message;
use super::phase::PreparedToolCall;
use super::phase::ToolBatchState;
use super::tool_call::start_tool_call;

/// 单批工具的最大并发数（有界并行，ALR-301）。
const MAX_PARALLEL_TOOLS: usize = 8;

/// 工具批次执行结果。
pub(super) enum ToolBatchOutcome {
    /// 批次全部闭合（成功/失败/拒绝/跳过均为合法结果），回到 `NeedModel`：
    /// 拒绝结果已写入会话，模型下一轮可见，可解释结束或换路径；是否允许
    /// 完成仍由完成门控判定（必需义务被拒时不会虚假成功，ALR-307）。
    Completed,
    /// 中断类命令（取消/关闭/引导）到达：批次已收敛闭合，命令交还执行驱动。
    Interrupted(Deferred),
}

/// 内置统一交互工具名：审批/确认/选择/输入经 request_user 发起（阻塞等待
/// 用户——与等待 LLM 同为 turn task 的外部 IO），响应作为该 Tool Call 的
/// 唯一结果写回（方案 §2）。
pub(crate) const REQUEST_USER_TOOL: &str = "request_user";

/// request_user 的工具规格（六 kind；超时由宿主统一定，模型不可自定义）。
pub(crate) fn request_user_tool_spec() -> crate::model::ToolSpec {
    crate::model::ToolSpec {
        name: REQUEST_USER_TOOL.to_string(),
        description: "向用户发起限时审批、确认、选择或输入请求。调用后当前执行暂停，用户响应将作为该工具调用的结果返回。需要用户决策或补充信息时使用；本工具必须独占一个工具调用批次，不要与其他工具同时调用。".to_string(),
        input_schema: serde_json::from_str(
            r#"{
  "type": "object",
  "properties": {
    "kind": {"type": "string", "enum": ["approval", "confirm", "choice", "multi_choice", "input", "form"]},
    "title": {"type": "string"},
    "description": {"type": "string"},
    "options": {"type": "array", "items": {}, "description": "choice/multi_choice 的候选项"},
    "fields": {"type": "array", "items": {}, "description": "form 的字段定义"},
    "question": {"type": "string", "description": "confirm 的问题文案"},
    "approval_challenge": {"type": "string", "description": "工具返回 approval_required 时的 challenge_id"}
  },
  "required": ["kind", "title"]
}"#,
        )
        .expect("request_user 输入 schema 必须是合法 JSON"),
    }
}

/// 解析 request_user 参数（kind/title/description/payload/挑战 ID）。
fn parse_request_user_args(
    value: &serde_json::Value,
) -> Result<
    (
        crate::interaction::InteractionRequestKind,
        String,
        String,
        String,
        Option<String>,
    ),
    String,
> {
    use crate::interaction::InteractionRequestKind;
    let kind = value
        .get("kind")
        .and_then(|item| item.as_str())
        .ok_or("缺少 kind")?;
    let kind = match kind {
        "approval" => InteractionRequestKind::Approval,
        "confirm" => InteractionRequestKind::Confirm,
        "choice" => InteractionRequestKind::Choice,
        "multi_choice" => InteractionRequestKind::MultiChoice,
        "input" => InteractionRequestKind::Input,
        "form" => InteractionRequestKind::Form,
        other => return Err(format!("非法 kind：{other}")),
    };
    let title = value
        .get("title")
        .and_then(|item| item.as_str())
        .unwrap_or("需要您的输入")
        .to_string();
    let description = value
        .get("description")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    let payload_field = match kind {
        InteractionRequestKind::Choice | InteractionRequestKind::MultiChoice => "options",
        InteractionRequestKind::Form => "fields",
        _ => "question",
    };
    let payload = value
        .get(payload_field)
        .cloned()
        .map(|item| item.to_string())
        .unwrap_or_else(|| "null".to_string());
    let challenge = value
        .get("approval_challenge")
        .and_then(|item| item.as_str())
        .map(str::to_string);
    Ok((kind, title, description, payload, challenge))
}

/// 已就绪待执行的单个工具调用（自 phase.rs 迁入，随流水线归属 tools.rs）。
pub(super) struct RunningToolCall {
    pub(super) tool: PreparedToolCall,
    pub(super) started_at: std::time::Instant,
}

/// 工具任务输出。
pub(super) struct ToolTaskOutput {
    pub(super) result: crate::tool::ToolResult,
    pub(super) duration_ms: u64,
}

/// 执行一个完整的工具批次（准备 → 审批 → 并行执行 → 收尾）。
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_tool_batch(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    trust_mode: &mut TrustMode,
    plugins: &[Arc<dyn Plugin>],
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    context_limit: usize,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    mut batch: ToolBatchState,
) -> ToolBatchOutcome {
    let mut tasks: JoinSet<ToolTaskOutput> = JoinSet::new();
    let mut running: HashMap<TaskId, RunningToolCall> = HashMap::new();
    // 已就绪待启动（受并发上限约束）与已完成待按序提交的结果。
    let mut pending_ready: std::collections::VecDeque<PreparedToolCall> =
        std::collections::VecDeque::new();
    let mut completed_buffer: Vec<(usize, RunningToolCall, ToolTaskOutput)> = Vec::new();

    // request_user 独占批次（方案 §15）：与其他调用同批时，其余调用不执行，
    // 写入明确未执行结果（模型在交互结束后可重新发起）。
    if batch
        .calls
        .iter()
        .any(|(_, call)| call.name == REQUEST_USER_TOOL)
    {
        let mut remaining = std::collections::VecDeque::new();
        while let Some((index, call)) = batch.calls.pop_front() {
            if call.name == REQUEST_USER_TOOL {
                remaining.push_back((index, call));
                continue;
            }
            let note =
                "request_user 必须独占一个工具调用批次，本调用未执行，请在交互结束后重新发起。"
                    .to_string();
            append_tool_result_message(&mut ctx.session, &call.id, &call.name, note.clone(), true);
            let _ = stream_tx.send(StreamEvent::ToolResult {
                name: call.name.clone(),
                tool_call_id: Some(call.id.clone()),
                ok: false,
                output: note,
                full_output: None,
                duration_ms: None,
            });
        }
        batch.calls = remaining;
        ctx.session.persist_to_disk();
    }

    'pipeline: loop {
        // ── 准备：逐个弹出待处理调用（校验/去重/权限分流）──
        while let Some((index, call)) = batch.calls.pop_front() {
            match prepare_tool_call(ctx, &call, &mut state.tool_history) {
                ToolPreflightOutcome::Skip { needs_recovery } => {
                    batch.needs_failure_recovery |= needs_recovery;
                    ctx.session.persist_to_disk();
                }
                ToolPreflightOutcome::Execute {
                    args_summary,
                    dedupe_key,
                } => {
                    if !batch.prepared_keys.insert(dedupe_key.clone()) {
                        record_parallel_duplicate_tool_call(ctx, &call);
                        continue;
                    }
                    let tool = PreparedToolCall {
                        index,
                        call,
                        args_summary,
                        dedupe_key,
                    };
                    // request_user：统一用户交互工具（审批/确认/选择/输入）。
                    // 与等待 LLM 同为 turn task 的外部 IO：阻塞等待用户响应，
                    // 响应/超时/取消作为本 Tool Call 的唯一结果写回（方案 §2）。
                    if tool.call.name == REQUEST_USER_TOOL {
                        match create_request_user(ctx, stream_tx, &tool) {
                            Ok((request, challenge)) => {
                                match wait_request_user(
                                    ctx,
                                    state,
                                    injections,
                                    trust_mode,
                                    plugins,
                                    stream_tx,
                                    context_limit,
                                    cmd_rx,
                                    &request,
                                    &challenge,
                                    &tool,
                                )
                                .await
                                {
                                    RequestUserOutcome::Answered { result } => {
                                        let tool_result = crate::tool::ToolResult {
                                            ok: true,
                                            summary: result,
                                            stdout: String::new(),
                                            stderr: String::new(),
                                            exit_code: 0,
                                            execution: None,
                                        };
                                        record_completed_tool_call(
                                            ctx,
                                            CompletedToolCall {
                                                call: &tool.call,
                                                args_summary: &tool.args_summary,
                                                dedupe_key: tool.dedupe_key.clone(),
                                                result: &tool_result,
                                                duration_ms: 0,
                                            },
                                            &mut state.tool_history,
                                        );
                                    }
                                    RequestUserOutcome::Failed { reason } => {
                                        append_tool_result_message(
                                            &mut ctx.session,
                                            &tool.call.id,
                                            REQUEST_USER_TOOL,
                                            reason.clone(),
                                            true,
                                        );
                                        let _ = stream_tx.send(StreamEvent::ToolResult {
                                            name: REQUEST_USER_TOOL.to_string(),
                                            tool_call_id: Some(tool.call.id.clone()),
                                            ok: false,
                                            output: reason,
                                            full_output: None,
                                            duration_ms: None,
                                        });
                                        ctx.session.persist_to_disk();
                                    }
                                    RequestUserOutcome::Interrupted(deferred_command) => {
                                        close_pending_calls(
                                            ctx,
                                            stream_tx,
                                            "工具调用因用户发送新消息而中断。",
                                        );
                                        return ToolBatchOutcome::Interrupted(deferred_command);
                                    }
                                }
                            }
                            Err(message) => {
                                append_tool_result_message(
                                    &mut ctx.session,
                                    &tool.call.id,
                                    REQUEST_USER_TOOL,
                                    message.clone(),
                                    true,
                                );
                                let _ = stream_tx.send(StreamEvent::ToolResult {
                                    name: REQUEST_USER_TOOL.to_string(),
                                    tool_call_id: Some(tool.call.id.clone()),
                                    ok: false,
                                    output: message,
                                    full_output: None,
                                    duration_ms: None,
                                });
                                ctx.session.persist_to_disk();
                            }
                        }
                        continue;
                    }
                    // 审批挑战驱动（方案 §12/§13；Supervised 即 Always 策略）：
                    // 无授权的受保护工具不执行，返回 approval_required 挑战，
                    // 模型据此发起 request_user(approval) 征询用户。
                    let trust_mode_label = format!("{trust_mode:?}");
                    if *trust_mode == TrustMode::FullTrust {
                        ctx.observer.audit_permission(
                            &ctx.session.id,
                            &tool.call.name,
                            "approved",
                            &trust_mode_label,
                            (!tool.args_summary.is_empty()).then_some(tool.args_summary.as_str()),
                        );
                        batch.ready_tools.push(tool);
                        continue;
                    }
                    let hub = crate::shared_runtime::interactions();
                    let arguments_hash = tool.call.arguments.to_string();
                    if hub
                        .grants
                        .try_consume(&ctx.session.id, "", &tool.call.name, &arguments_hash)
                    {
                        ctx.observer.audit_permission(
                            &ctx.session.id,
                            &tool.call.name,
                            "approved_by_grant",
                            &trust_mode_label,
                            (!tool.args_summary.is_empty()).then_some(tool.args_summary.as_str()),
                        );
                        batch.ready_tools.push(tool);
                        continue;
                    }
                    let challenge = hub.challenges.create(
                        &ctx.session.id,
                        "",
                        &tool.call.name,
                        arguments_hash,
                        tool.args_summary.clone(),
                    );
                    ctx.observer.audit_permission(
                        &ctx.session.id,
                        &tool.call.name,
                        "needs_approval",
                        &trust_mode_label,
                        (!tool.args_summary.is_empty()).then_some(tool.args_summary.as_str()),
                    );
                    append_tool_result_message(
                        &mut ctx.session,
                        &tool.call.id,
                        &tool.call.name,
                        challenge.to_tool_payload(),
                        true,
                    );
                    let _ = stream_tx.send(StreamEvent::ToolResult {
                        name: tool.call.name.clone(),
                        tool_call_id: Some(tool.call.id.clone()),
                        ok: false,
                        output: challenge.to_tool_payload(),
                        full_output: None,
                        duration_ms: None,
                    });
                    ctx.session.persist_to_disk();
                }
            }
        }

        // ── 执行：有界并行启动（≤MAX_PARALLEL_TOOLS），结果按模型声明顺序提交 ──
        pending_ready.extend(std::mem::take(&mut batch.ready_tools));
        launch_ready_tools(ctx, &mut pending_ready, &mut tasks, &mut running);
        if tasks.is_empty() {
            flush_ordered_results(ctx, state, &mut batch, &mut completed_buffer, usize::MAX);
            break 'pipeline;
        }
        let joined = tokio::select! {
            biased;
            command = cmd_rx.recv() => {
                let command = match command {
                    Some(command) => command,
                    None => {
                        flush_ordered_results(
                            ctx,
                            state,
                            &mut batch,
                            &mut completed_buffer,
                            usize::MAX,
                        );
                        abort_running_tools(ctx, &mut tasks, &mut running, stream_tx).await;
                        close_pending_calls(ctx, stream_tx, "工具调用因用户发送新消息而中断。");
                        return ToolBatchOutcome::Interrupted(Deferred::Closed);
                    }
                };
                if is_interrupting(&command) {
                    flush_ordered_results(
                        ctx,
                        state,
                        &mut batch,
                        &mut completed_buffer,
                        usize::MAX,
                    );
                    abort_running_tools(ctx, &mut tasks, &mut running, stream_tx).await;
                    close_pending_calls(ctx, stream_tx, "工具调用因用户发送新消息而中断。");
                    return ToolBatchOutcome::Interrupted(Deferred::Command(command));
                }
                handle_ambient_command(
                    ctx,
                    state,
                    injections,
                    trust_mode,
                    plugins,
                    stream_tx,
                    context_limit,
                    command,
                );
                continue;
            }
            joined = tasks.join_next_with_id() => joined,
        };
        let joined = joined.expect("工具任务集合非空时必须返回结果");
        let (task_id, task_output) = match joined {
            Ok((task_id, output)) => (task_id, output),
            Err(error) => {
                let task_id = error.id();
                let running_record = running.get(&task_id).expect("异常工具任务必须存在运行记录");
                let message = format!("工具任务异常结束：{error}");
                (
                    task_id,
                    ToolTaskOutput {
                        result: crate::tool::ToolResult {
                            ok: false,
                            summary: message.clone(),
                            stdout: String::new(),
                            stderr: message,
                            exit_code: 1,
                            execution: None,
                        },
                        duration_ms: running_record.started_at.elapsed().as_millis() as u64,
                    },
                )
            }
        };
        let running_record = running
            .remove(&task_id)
            .expect("完成的工具任务必须存在运行记录");
        completed_buffer.push((running_record.tool.index, running_record, task_output));
        // 提交边界：index 小于仍在途（运行或待启动）的最小 index 的结果
        // 按 model 声明顺序落库（ALR-301）。
        let inflight_min = running
            .values()
            .map(|record| record.tool.index)
            .chain(pending_ready.iter().map(|tool| tool.index))
            .min()
            .unwrap_or(usize::MAX);
        flush_ordered_results(ctx, state, &mut batch, &mut completed_buffer, inflight_min);
        launch_ready_tools(ctx, &mut pending_ready, &mut tasks, &mut running);
    }

    // ── 收尾：无效调用上下文、失败恢复提示、观测压力记录 ──
    append_invalid_tool_calls_context(ctx, &batch.invalid_tool_calls);
    if batch.needs_failure_recovery {
        let request_tools = ctx.tools.clone();
        append_failure_recovery_prompt(ctx, &state.tool_history, &request_tools);
    } else {
        ctx.session.persist_to_disk();
    }
    let observed_tokens = observed_total_tokens(&batch.response_usage);
    state.last_observed_tokens = observed_tokens;
    ctx.session.current_tokens = observed_tokens;
    ToolBatchOutcome::Completed
}

/// 中断类命令：取消/关闭/引导消息——批次收敛闭合后交还执行驱动。
fn is_interrupting(command: &Command) -> bool {
    matches!(
        command,
        Command::Cancel | Command::Shutdown | Command::InjectUserMessage { .. }
    )
}

/// 就地消化非中断命令的运行时副作用（与统一命令处理的 KeepCurrent 类一致）。
#[allow(clippy::too_many_arguments)]
fn handle_ambient_command(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    trust_mode: &mut TrustMode,
    plugins: &[Arc<dyn Plugin>],
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    context_limit: usize,
    command: Command,
) {
    match command {
        // ResolveInteraction 在批次外到达（无活跃等待）：由注册表判定迟到，丢弃
        Command::ResolveInteraction { .. } => {}
        Command::SetTrustMode(mode) => {
            set_runtime_trust_mode(trust_mode, plugins, mode);
        }
        Command::SetReasoningEffort(effort) => {
            ctx.agent_config.reasoning_effort = effort.clone();
            ctx.session.reasoning_effort = Some(effort);
        }
        Command::SetTitle {
            title,
            only_if_default,
        } => {
            if !only_if_default || crate::core::is_default_title(&ctx.session.title) {
                ctx.session.title = title.clone();
                ctx.session.updated_at = tiangong_types::now_text();
                let _ = stream_tx.send(StreamEvent::TitleChanged { title });
            }
        }
        Command::InjectTool { tool_name, payload } => {
            injections.receive(stream_tx, tool_name, payload);
        }
        Command::EmitStreamEvent(event) => {
            let _ = stream_tx.send(*event);
        }
        Command::ReportUsage {
            usage,
            source,
            emit_event,
        } => {
            record_plugin_usage(
                stream_tx,
                context_limit,
                &mut state.accumulated_usage,
                usage,
                source,
                emit_event,
            );
        }
        Command::CompressContext | Command::ResetContext => {
            // Driver 在 turn 结束后的空闲边界执行维护命令。
        }
        // 迟到或不匹配的审批：明确忽略。
        Command::Cancel | Command::Shutdown => {}
        Command::InjectUserMessage { .. } => unreachable!("中断类命令已在上方分流"),
    }
}

/// 在并发上限内启动就绪工具。
fn launch_ready_tools(
    ctx: &mut TurnContext,
    pending_ready: &mut std::collections::VecDeque<PreparedToolCall>,
    tasks: &mut JoinSet<ToolTaskOutput>,
    running: &mut HashMap<TaskId, RunningToolCall>,
) {
    while running.len() < MAX_PARALLEL_TOOLS
        && let Some(tool) = pending_ready.pop_front()
    {
        start_tool_execution(ctx, tool, tasks, running);
    }
}

/// 按 model 声明顺序提交已完成结果：提交所有 index < `boundary` 的缓冲项。
fn flush_ordered_results(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    batch: &mut ToolBatchState,
    completed_buffer: &mut Vec<(usize, RunningToolCall, ToolTaskOutput)>,
    boundary: usize,
) {
    completed_buffer.sort_by_key(|(index, _, _)| *index);
    let mut needs_recovery = false;
    while completed_buffer
        .first()
        .is_some_and(|(index, _, _)| *index < boundary)
    {
        let (_, running_record, task_output) = completed_buffer.remove(0);
        needs_recovery |= record_completed_tool_call(
            ctx,
            CompletedToolCall {
                call: &running_record.tool.call,
                args_summary: &running_record.tool.args_summary,
                dedupe_key: running_record.tool.dedupe_key,
                result: &task_output.result,
                duration_ms: task_output.duration_ms,
            },
            &mut state.tool_history,
        );
    }
    batch.needs_failure_recovery |= needs_recovery;
}

/// 启动一个已就绪的工具任务（ToolStart 事件 + JoinSet 注册）。
pub(super) fn start_tool_execution(
    ctx: &mut TurnContext,
    tool: PreparedToolCall,
    tasks: &mut JoinSet<ToolTaskOutput>,
    running: &mut HashMap<TaskId, RunningToolCall>,
) {
    let _ = ctx.stream_tx.send(StreamEvent::ToolStart {
        name: tool.call.name.clone(),
        args_summary: tool.args_summary.clone(),
    });
    let actor_id = ctx.session.id.clone();
    let future = start_tool_call(ctx, &tool.call, &actor_id);
    let started_at = std::time::Instant::now();
    let task = tasks.spawn(async move {
        let result = future.await;
        ToolTaskOutput {
            result,
            duration_ms: started_at.elapsed().as_millis() as u64,
        }
    });
    running.insert(task.id(), RunningToolCall { tool, started_at });
}

/// 中断时收敛运行中的工具任务：全部中止并按声明顺序闭合协议。
async fn abort_running_tools(
    ctx: &mut TurnContext,
    tasks: &mut JoinSet<ToolTaskOutput>,
    running: &mut HashMap<TaskId, RunningToolCall>,
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
) {
    tasks.shutdown().await;
    let mut interrupted = running.drain().map(|(_, tool)| tool).collect::<Vec<_>>();
    interrupted.sort_by_key(|running_call| running_call.tool.index);
    let mut interrupted_events = Vec::with_capacity(interrupted.len());
    for running_call in interrupted {
        let duration_ms = running_call.started_at.elapsed().as_millis() as u64;
        let output = "工具调用因用户发送新消息而中断。".to_string();
        append_tool_result_message(
            &mut ctx.session,
            &running_call.tool.call.id,
            &running_call.tool.call.name,
            output.clone(),
            true,
        );
        interrupted_events.push(StreamEvent::ToolResult {
            name: running_call.tool.call.name.clone(),
            tool_call_id: Some(running_call.tool.call.id.clone()),
            ok: false,
            output,
            full_output: None,
            duration_ms: Some(duration_ms),
        });
    }
    ctx.session.persist_to_disk();
    for event in interrupted_events {
        let _ = stream_tx.send(event);
    }
}

/// 闭合批次中尚未执行的悬空调用（模型已返回但工具未启动）。
fn close_pending_calls(
    ctx: &mut TurnContext,
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    reason: &str,
) {
    let closed = ctx.session.close_unfinished_tool_calls_with_reason(reason);
    for (tool_call_id, tool_name, output) in closed {
        let _ = stream_tx.send(StreamEvent::ToolResult {
            name: tool_name,
            tool_call_id: Some(tool_call_id),
            ok: false,
            output,
            full_output: None,
            duration_ms: None,
        });
    }
}

/// request_user 等待结论。
enum RequestUserOutcome {
    /// 用户已响应：结果负载（含审批授权生成）。
    Answered { result: String },
    /// 超时/取消/闭合异常：按失败结果闭合。
    Failed { reason: String },
    /// 中断类命令打断。
    Interrupted(Deferred),
}

/// 创建 request_user 交互请求：解析参数 → approval 消费挑战（取得真实审批
/// 目标，不信 Agent 报文）→ 登记注册表 → 发事件。返回 (请求, 审批目标)。
fn create_request_user(
    ctx: &mut TurnContext,
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    tool: &PreparedToolCall,
) -> std::result::Result<
    (
        crate::interaction::InteractionRequest,
        Option<crate::interaction::ApprovalChallenge>,
    ),
    String,
> {
    use crate::interaction::{InteractionRequest, InteractionRequestKind};

    let value = serde_json::from_str::<serde_json::Value>(&tool.call.arguments.to_string())
        .map_err(|error| format!("request_user 参数不是合法 JSON：{error}"))?;
    let (kind, title, description, payload, challenge_id) = parse_request_user_args(&value)?;

    let hub = crate::shared_runtime::interactions();
    let challenge = if kind == InteractionRequestKind::Approval {
        let challenge = match challenge_id {
            Some(id) => hub.challenges.take(&id),
            // 静态集成 Mock 无法把动态 challenge_id 回填到下一次 Tool Call；
            // 仅测试构建保留适配，生产构建必须显式携带 ID。
            #[cfg(test)]
            None => hub.challenges.take_latest_of_session(&ctx.session.id),
            #[cfg(not(test))]
            None => {
                return Err(
                    "approval 请求必须携带原工具 approval_required 报文中的 approval_challenge"
                        .to_string(),
                );
            }
        };
        match challenge {
            Some(challenge) if challenge.session_id == ctx.session.id => Some(challenge),
            Some(_) => {
                return Err("审批挑战不属于当前会话".to_string());
            }
            None => {
                return Err("审批挑战无效或已过期：请先调用原工具获取 approval_required 报文中的 challenge_id 后重试".to_string());
            }
        }
    } else {
        None
    };

    let request = hub.registry.create(InteractionRequest {
        request_id: scru128::new().to_string(),
        session_id: ctx.session.id.clone(),
        source_message_id: None,
        tool_call_id: tool.call.id.clone(),
        kind,
        title: title.clone(),
        description: description.clone(),
        payload: payload.clone(),
        approval_challenge: None,
        status: crate::interaction::InteractionStatus::Pending,
        created_at: chrono::Local::now().naive_local(),
        deadline: chrono::Local::now().naive_local(),
    });
    let _ = stream_tx.send(StreamEvent::InteractionRequested {
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
        tool_call_id: request.tool_call_id.clone(),
        kind: kind.as_str().to_string(),
        title,
        description,
        payload,
        created_at: request
            .created_at
            .format("%Y-%m-%dT%H:%M:%S%.f")
            .to_string(),
        deadline: request.deadline.format("%Y-%m-%dT%H:%M:%S%.f").to_string(),
    });
    Ok((request, challenge))
}

/// 阻塞等待 request_user 结果：响应命令/超时/中断三类出口；其余命令就地消化。
/// 超时按绝对 deadline（注册表复核）fail-closed；审批批准时按挑战真实目标
/// 生成授权（方案 §9/§12）。
#[allow(clippy::too_many_arguments)]
async fn wait_request_user(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    trust_mode: &mut TrustMode,
    plugins: &[Arc<dyn Plugin>],
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    context_limit: usize,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    request: &crate::interaction::InteractionRequest,
    challenge: &Option<crate::interaction::ApprovalChallenge>,
    tool: &PreparedToolCall,
) -> RequestUserOutcome {
    let request_id = request.request_id.clone();
    let hub = crate::shared_runtime::interactions();
    let deadline = tokio::time::Instant::now()
        + (request.deadline - chrono::Local::now().naive_local())
            .to_std()
            .unwrap_or(std::time::Duration::from_millis(1));
    let _ = tool;
    loop {
        let command = tokio::select! {
            command = cmd_rx.recv() => match command {
                Some(command) => command,
                None => {
                    let _ = hub.registry.cancel(&request_id, "命令通道关闭".to_string());
                    return RequestUserOutcome::Failed { reason: "等待被关闭".to_string() };
                }
            },
            _ = tokio::time::sleep_until(deadline) => {
                // 超时 fail-closed：注册表按绝对截止原子闭合（响应竞态时胜者唯一）
                return match hub.registry.expire(&request_id) {
                    crate::interaction::CloseOutcome::Won(closed) => {
                        let (payload, _) = crate::interaction::render_closed_tool_result(&closed);
                        let _ = stream_tx.send(StreamEvent::InteractionClosed {
                            request_id: request_id.clone(),
                            status: "expired".to_string(),
                        });
                        RequestUserOutcome::Failed { reason: payload }
                    }
                    // 已被响应闭合：响应命令应在通道中，继续接收
                    _ => continue,
                };
            }
        };
        match command {
            Command::ResolveInteraction { request: closed }
                if closed.request.request_id == request_id =>
            {
                // 审批批准：按挑战真实目标生成授权（approve_once 参数绑定 /
                // approve_for_runtime 跨 turn；拒绝/超时不产生授权）
                if let (Some(challenge), crate::interaction::ClosedOutcome::Answered { result }) =
                    (challenge, &closed.outcome)
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(result)
                    && let Some(decision) = value.get("decision").and_then(|d| d.as_str())
                {
                    match decision {
                        "approve_once" => hub.grants.grant_once(
                            &challenge.session_id,
                            &challenge.plugin_id,
                            &challenge.tool_name,
                            challenge.arguments_hash.clone(),
                        ),
                        "approve_for_runtime" => hub.grants.grant_runtime(
                            &challenge.session_id,
                            &challenge.plugin_id,
                            &challenge.tool_name,
                        ),
                        _ => {}
                    }
                }
                let (payload, ok) = crate::interaction::render_closed_tool_result(&closed);
                if ok {
                    return RequestUserOutcome::Answered { result: payload };
                }
                return RequestUserOutcome::Failed { reason: payload };
            }
            command if is_interrupting(&command) => {
                let _ = hub
                    .registry
                    .cancel(&request_id, "用户发送了新的消息".to_string());
                let _ = stream_tx.send(StreamEvent::InteractionClosed {
                    request_id,
                    status: "cancelled".to_string(),
                });
                return RequestUserOutcome::Interrupted(Deferred::Command(command));
            }
            command => {
                handle_ambient_command(
                    ctx,
                    state,
                    injections,
                    trust_mode,
                    plugins,
                    stream_tx,
                    context_limit,
                    command,
                );
            }
        }
    }
}
