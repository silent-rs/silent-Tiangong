use tiangong_types::{MessageRole, StreamEvent};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{core::Command, session::Session};

/// turn task:执行 TurnContext 中的 turn。
///
/// TurnContext(session + 用户消息已注入)在 deliver 中构建,传入此处执行。
/// turn 结束后 session 落盘,task 退出。
pub(crate) async fn run_turn(
    mut ctx: crate::turn_context::TurnContext,
    mut cmd_rx: UnboundedReceiver<Command>,
) {
    let stream_tx = ctx.stream_tx.clone();
    let turn_started = std::time::Instant::now();
    let turn_start_cwd = ctx.session.cwd.clone();

    for plugin in &ctx.plugins {
        plugin.on_turn_started(
            &mut ctx.session,
            ctx.session.latest_user_message_index().unwrap_or(0),
        );
    }

    let usage = ctx.execute_turn(&mut cmd_rx).await;

    // execute_turn 返回的 usage 直接应用到 session
    ctx.session.token_usage.accumulate(&usage);

    let mut terminal = StreamEvent::Done { usage: None };

    if ctx.session.cwd != turn_start_cwd {
        let workspace_path = std::path::PathBuf::from(&ctx.session.cwd);
        let workspace = workspace_path.is_dir().then_some(workspace_path.as_path());
        for plugin in &ctx.plugins {
            plugin.set_workspace(workspace);
        }
    }

    let interrupted_tools = {
        let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
        let result = crate::react::message::close_unfinished_tool_calls_for_turn(&mut session);
        ctx.session = session;
        result
    };
    if !interrupted_tools.is_empty() {
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
        terminal = StreamEvent::Error {
            message: "本轮仍有未完成的工具调用，已安全中断".to_string(),
        };
    }

    {
        let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
        crate::react::message::flush_deferred_tool_injections(&mut session, &ctx);
        ctx.session = session;
    }

    let elapsed_ms = turn_started.elapsed().as_millis() as u64;
    let mut status = TurnOutcome::from_terminal(&terminal).status;

    if status == tiangong_types::TurnStatus::Cancelled {
        for plugin in &ctx.plugins {
            plugin.on_cancel(&mut ctx.session).await;
        }
    }

    let mut user_msg_updated = false;
    if let Some(msg) = ctx
        .session
        .messages
        .iter_mut()
        .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
    {
        msg.set_turn_result(elapsed_ms, status);
        user_msg_updated = true;
    }
    for plugin in &ctx.plugins {
        plugin.on_turn_finished(&mut ctx.session, turn_start_idx);
    }

    ctx.session.clear_transient_content();

    if let Err(error) = ctx.session.try_persist_to_disk() {
        terminal = StreamEvent::Error {
            message: format!("最终会话持久化失败：{error}"),
        };
        status = tiangong_types::TurnStatus::Failed;
        if let Some(msg) = ctx
            .session
            .messages
            .iter_mut()
            .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
        {
            msg.set_turn_result(elapsed_ms, status);
        }
        let _ = ctx.session.try_persist_to_disk();
    }
    if user_msg_updated {
        crate::react::message::emit_session_message_upsert(&ctx.session, &ctx, &user_msg_id);
    }

    // 直接发送终态事件(无 forwarder 屏障)
    let _ = stream_tx.send(terminal);

    drop(ctx);
}

/// 转发线程捕获的单个对话轮次终态。
///
/// `status` 由终态事件推导：`Done` → Success；`Error` 文案含「取消/cancel/abort」
/// 时为 Cancelled，否则为 Failed。run_turn 据此把 `status` 与执行时长
/// 写入用户消息，供前端（含历史会话）展示。
struct TurnOutcome {
    status: tiangong_types::TurnStatus,
}

impl TurnOutcome {
    fn from_terminal(event: &StreamEvent) -> Self {
        let status = match event {
            StreamEvent::Done { .. } => tiangong_types::TurnStatus::Success,
            StreamEvent::Error { message } => {
                let lower = message.to_lowercase();
                if lower.contains("取消")
                    || lower.contains("cancel")
                    || lower.contains("abort")
                    || lower.contains("中断")
                {
                    tiangong_types::TurnStatus::Cancelled
                } else {
                    tiangong_types::TurnStatus::Failed
                }
            }
            _ => tiangong_types::TurnStatus::Failed,
        };
        Self { status }
    }
}
