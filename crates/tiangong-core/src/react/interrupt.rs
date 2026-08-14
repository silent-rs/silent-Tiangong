//! 引导/取消的结构化中断与协议闭合（任务 11，自 execute.rs 机械拆分）。

use crate::react::phase::{ActiveLlm, ExecutionPhase, LlmPurpose};
use crate::turn_context::TurnContext;
use tiangong_types::StreamEvent;

use super::cancel::{abort_and_join, emit_cancel_usage};
use super::execute::{
    AgentLoopState, ToolInjectionBuffer, persist_streamed_react_message, record_rejected_tool_call,
};
use super::message::append_tool_result_message;
use super::summary::persist_partial_summary;

/// 持久化被中断的 LLM 请求已流式收到的部分输出。
///
/// Summary 的部分输出降级为 React 过程消息（ALR-104）——半截总结不得保持最终
/// Summary 身份；ForceFinal 无可靠完整内容，丢弃不提交。
pub(super) fn persist_interrupted_llm_output(
    ctx: &mut TurnContext,
    purpose: &LlmPurpose,
    pending_msg_id: &str,
    streamed_text: &str,
    streamed_reasoning: &str,
) {
    match purpose {
        LlmPurpose::React { .. } | LlmPurpose::Summary { .. } => {
            persist_streamed_react_message(ctx, pending_msg_id, streamed_text, streamed_reasoning);
        }
        LlmPurpose::ForceFinal { .. } => {}
    }
}

/// 中断主循环直接拥有的活动（阶段感知）：消费当前阶段持有的资源并完成收尾。
///
/// `reason` 区分两种中断语义：
/// - 引导消息（ALR-101）：Summary 部分输出**降级**为 React 过程消息（ALR-104）；
/// - 取消/关闭：Summary 部分输出按取消路径持久化（保持 Summary 身份，与旧行为
///   一致），工具/压缩/审批处理相同。
///
/// 两者都**不取消插件独立持有的后台任务**（ALR-103）。中断后安装 `NeedModel`
/// 之外的目标阶段由调用方决定；本函数保证阶段资源全部转移、取消或完成（ALR-205）。
pub(super) async fn interrupt_active_work(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    context_limit: usize,
    downgrade_summary: bool,
) {
    let phase = state.take_phase();
    tracing::debug!(
        session_id = %ctx.session.id,
        from_phase = phase.name(),
        downgrade_summary,
        "中断主循环直接拥有的活动"
    );
    match phase {
        ExecutionPhase::WaitingModel(active)
        | ExecutionPhase::CheckingCompletion(active)
        | ExecutionPhase::ForceFinalPhase(active) => {
            let ActiveLlm {
                purpose,
                pending_msg_id,
                sink,
                task,
                streamed_text,
                streamed_reasoning,
                streaming_usage,
                ..
            } = active;
            sink.finish();
            abort_and_join(task).await;
            if downgrade_summary {
                persist_interrupted_llm_output(
                    ctx,
                    &purpose,
                    &pending_msg_id,
                    &streamed_text,
                    &streamed_reasoning,
                );
            } else {
                match purpose {
                    LlmPurpose::React { .. } => persist_streamed_react_message(
                        ctx,
                        &pending_msg_id,
                        &streamed_text,
                        &streamed_reasoning,
                    ),
                    LlmPurpose::Summary { .. } => persist_partial_summary(
                        ctx,
                        &pending_msg_id,
                        &streamed_text,
                        &streamed_reasoning,
                    ),
                    LlmPurpose::ForceFinal { .. } => {}
                }
            }
            emit_cancel_usage(stream_tx, &streaming_usage, context_limit);
            state.accumulated_usage.accumulate(&streaming_usage);
        }
        ExecutionPhase::WaitingTools(mut tools) => {
            tools.tasks.shutdown().await;
            let mut interrupted = tools
                .running
                .drain()
                .map(|(_, tool)| tool)
                .collect::<Vec<_>>();
            interrupted.sort_by_key(|running| running.tool.index);
            let mut interrupted_events = Vec::with_capacity(interrupted.len());
            for running in interrupted {
                let duration_ms = running.started_at.elapsed().as_millis() as u64;
                let output = "工具调用因用户发送新消息而中断。".to_string();
                append_tool_result_message(
                    &mut ctx.session,
                    &running.tool.call.id,
                    &running.tool.call.name,
                    output.clone(),
                    true,
                );
                interrupted_events.push(StreamEvent::ToolResult {
                    name: running.tool.call.name,
                    tool_call_id: Some(running.tool.call.id),
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
            // 批次中尚未执行的调用一并闭合。
            let closed = ctx
                .session
                .close_unfinished_tool_calls_with_reason("工具调用因用户发送新消息而中断。");
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
        ExecutionPhase::Compressing(compressing) => {
            compressing.active.cancel(ctx).await;
        }
        ExecutionPhase::WaitingApproval(approval) => {
            record_rejected_tool_call(
                ctx,
                &approval.pending.tool.call,
                &approval.pending.tool.args_summary,
            );
        }
        ExecutionPhase::PreparingTools(_) | ExecutionPhase::PendingFinish(_) => {
            // 无在途活动；PreparingTools 的悬空调用由下方统一闭合。
        }
        ExecutionPhase::NeedModel | ExecutionPhase::StartCheckingCompletion => {}
        ExecutionPhase::StartForceFinal { .. } => {}
    }

    injections.commit(ctx);

    // 闭合残留的未完成 tool calls（模型已返回但工具未开始执行的）。
    let closed = ctx
        .session
        .close_unfinished_tool_calls_with_reason("工具调用因用户发送新消息而中断。");
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
