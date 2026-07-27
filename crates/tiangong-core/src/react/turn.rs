//! 单个 turn 的生命周期。
//!
//! [`TurnContext`] 定义在 `crate::turn_context`,是 turn 级能力容器。本文件负责
//! turn 的启动、插件回调、状态提交与最终持久化。

use tokio::sync::mpsc as tokio_mpsc;

use crate::core::command::Command;
use crate::session::MessageRole;
use crate::turn_context::TurnContext;
use tiangong_types::StreamEvent;

use super::execute::execute_turn;
use super::outcome::TurnExecutionOutcome;
use super::timer::TurnElapsedTimer;

/// 执行并收尾一个完整的 turn task。
///
/// `deliver` 已完成用户消息接收并构建 [`TurnContext`]；本函数依次负责插件生命周期、
/// Agent Loop、消息协议收尾、轮次状态提交和最终持久化。
pub(crate) async fn run_turn(
    mut ctx: TurnContext,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
) {
    // ── 固定轮次锚点 ──
    // 当前用户消息已在 deliver 阶段写入 Session。后续插件回调、耗时与状态都使用
    // 同一消息索引和 ID，避免执行过程中追加的消息改变本轮归属。
    let stream_tx = ctx.stream_tx.clone();
    let turn_started = std::time::Instant::now();
    let Some(turn_start_idx) = ctx.session.latest_user_message_index() else {
        let _ = stream_tx.send(StreamEvent::Error {
            message: "本轮 Session 缺少用户消息".to_string(),
        });
        return;
    };
    let user_msg_id = ctx.session.messages[turn_start_idx].id.clone();
    let elapsed_timer = TurnElapsedTimer::start(turn_started, stream_tx.clone());

    // ── 启动插件生命周期 ──
    // 插件看到的是已包含本轮用户消息的完整 Session。
    for plugin in &ctx.plugins {
        plugin.on_turn_started(&mut ctx.session, turn_start_idx);
    }

    // ── 执行 Agent Loop ──
    // execute_turn 返回明确的执行结果和累计用量，不在内部发送终态事件。
    let execution = execute_turn(&mut ctx, &mut cmd_rx).await;
    // Agent Loop 结束后，本函数的收尾阶段（消息协议修复、插件回调、持久化）不再
    // 消费命令通道。显式 drop 接收端，使所有 PluginFeedbackTx 的 is_closed() 立即
    // 返回 true——否则 turn task 退出前会出现"通道未关闭但已无人消费"的窗口，
    // 此时 watcher 经 inject_tool 投递的终端命令会成功入队但随队列销毁而丢失。
    drop(cmd_rx);
    let usage = execution.usage;
    let mut outcome = execution.outcome;
    ctx.session.token_usage.accumulate(&usage);

    // ── 修复消息协议 ──
    // 先为悬空的 tool_call 补齐失败结果，保证 Provider 历史满足
    // Assistant(tool_call) -> Tool(result) 的配对要求。
    let interrupted_tools = ctx
        .session
        .close_unfinished_tool_calls_with_reason("工具调用因本轮结束而中断，未执行。");
    let had_interrupted_tools = !interrupted_tools.is_empty();
    for (tool_call_id, tool_name, output) in interrupted_tools {
        let _ = stream_tx.send(StreamEvent::ToolResult {
            name: tool_name,
            tool_call_id: Some(tool_call_id),
            ok: false,
            output,
            full_output: None,
            duration_ms: None,
        });
    }
    if had_interrupted_tools && matches!(outcome, TurnExecutionOutcome::Success) {
        outcome = TurnExecutionOutcome::Failed("本轮仍有未完成的工具调用，已安全中断".to_string());
    }

    // ── 提交轮次状态与插件收尾 ──
    // 先把明确结果写入本轮用户消息，让取消钩子和结束钩子看到一致状态。
    elapsed_timer.stop().await;
    let elapsed_ms = turn_started.elapsed().as_millis() as u64;
    let status = outcome.status();
    if let Some(msg) = ctx
        .session
        .messages
        .iter_mut()
        .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
    {
        msg.set_turn_result(elapsed_ms, status);
    }
    if status == tiangong_types::TurnStatus::Cancelled {
        for plugin in &ctx.plugins {
            plugin.on_cancel(&mut ctx.session).await;
        }
    }
    // on_turn_finished 使用与 on_turn_started 相同的起点，并可在最终落盘前处理
    // 本轮新增消息（例如建立索引或提交记忆任务）。
    for plugin in &ctx.plugins {
        plugin.on_turn_finished(&mut ctx.session, turn_start_idx);
    }

    // ── 清理运行态并最终持久化 ──
    // base64 等瞬态内容只用于本轮模型请求，不能进入磁盘会话合同。
    ctx.session.clear_transient_content();

    if let Err(error) = ctx.session.try_persist_to_disk() {
        // 最终落盘失败必须把本轮降级为 Failed，并带着失败状态再尝试保存一次。
        outcome = TurnExecutionOutcome::Failed(format!("最终会话持久化失败：{error}"));
        if let Some(msg) = ctx
            .session
            .messages
            .iter_mut()
            .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
        {
            msg.set_turn_result(elapsed_ms, tiangong_types::TurnStatus::Failed);
        }
        let _ = ctx.session.try_persist_to_disk();
    }

    // ── 发布唯一终态 ──
    // 宿主在收到终态后从磁盘重载权威 Session，不再需要额外的最终消息快照。
    let _ = stream_tx.send(outcome.terminal_event(usage));
}
