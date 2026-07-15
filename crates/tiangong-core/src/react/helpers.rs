//! ReAct 主循环的通用辅助函数。
//!
//! 这些函数不依赖 `TurnContext` 自身状态（无 `&self`），而是操作传入的
//! `Session` / `TurnContext` / 命令通道，属于与主状态机解耦的纯过程性逻辑：
//! - 命令排空（`drain_pending_commands_async`）
//! - 非阻塞取消检查（`check_cancel`）
//! - 最终回答启发式判断（`looks_like_final_answer`）
//!
//! 浏览器页面自动观察已随 PageFetcher 能力下沉迁入 browser 插件（#225），
//! core 不再感知浏览器快照注入。

use std::sync::mpsc::Sender as StdSender;

use tokio::sync::mpsc as tokio_mpsc;

use crate::core::command::{Command, PendingCommandEffect};
use crate::react::message::accept_runtime_user_message;
use crate::session::Session;
use crate::turn_context::TurnContext;
use tiangong_types::StreamEvent;

/// 判断 ReAct 阶段的文本回复是否「看起来像一个完整回答」（而非向用户提问）。
///
/// 用于智能提升：当本轮执行过工具、但模型已给出实质文本，且不像是反问用户
/// （以问号结尾或显式请求输入）时，直接把它作为最终回复，避免总结阶段再生成
/// 一个更精简、反而丢失细节的版本。
///
/// 判据纯粹基于「是否在向用户提问」的语义，不依赖长度阈值——任务完成的简短
/// 确认（如「已创建定时提醒：每天 9 点叫你起床。」）同样是合法的最终回复。
pub(super) fn looks_like_final_answer(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    // 以问号结尾 → 多为向用户提问，不作为最终回答。
    if trimmed.ends_with('?') || trimmed.ends_with('？') {
        return false;
    }
    // 显式请求用户提供信息：仅在文本以这些短语开头时才排除。
    // （较长文本里偶尔出现这些词通常是正常叙述，不应误判。）
    let intro = trimmed.chars().take(16).collect::<String>();
    let ask_intro = [
        "请问",
        "请提供",
        "请确认",
        "请选择",
        "你想",
        "你希望",
        "你是否",
    ];
    if ask_intro.iter().any(|p| intro.starts_with(p)) {
        return false;
    }
    true
}

/// 非阻塞排空命令队列，处理排队的用户命令（消息注入/取消/上下文压缩等）。
pub(super) fn drain_pending_commands_async(
    session: &mut Session,
    ctx: &TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> PendingCommandEffect {
    let commands = std::iter::from_fn(|| cmd_rx.try_recv().ok());
    process_commands(session, ctx, commands)
}

/// 处理工具执行期间暂存的命令；工具结果闭合后再调用以保持 Provider 消息顺序。
pub(super) fn process_buffered_commands(
    session: &mut Session,
    ctx: &TurnContext,
    commands: Vec<Command>,
) -> PendingCommandEffect {
    process_commands(session, ctx, commands)
}

fn process_commands(
    session: &mut Session,
    ctx: &TurnContext,
    commands: impl IntoIterator<Item = Command>,
) -> PendingCommandEffect {
    let mut current_agent_input = None;

    for cmd in commands {
        match cmd {
            Command::Cancel => {
                let _ = ctx.stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return PendingCommandEffect::Terminate;
            }
            Command::Shutdown => return PendingCommandEffect::Shutdown,
            Command::Message {
                prepared,
                message_id,
            } => match accept_runtime_user_message(session, ctx, message_id, prepared) {
                Ok(text) => current_agent_input = Some(text),
                Err(err) => tracing::warn!(
                    error = %err,
                    "排空队列时追加用户消息持久化失败"
                ),
            },
            Command::Approval { .. } => {}
            Command::InjectTool { tool_name, payload } => {
                crate::react::message::defer_tool_injection(session, ctx, tool_name, payload);
            }
            Command::CompressContext => {
                let _ = ctx.stream_tx.send(StreamEvent::AgentNotification {
                    agent_id: "system".to_string(),
                    agent_label: "系统".to_string(),
                    content: "当前轮次执行中，已跳过手动压缩，请在轮次结束后重试".to_string(),
                    level: "warning".to_string(),
                });
            }
            Command::ResetContext => {
                crate::core::reset_context_for_session(session, ctx);
            }
            Command::EmitStreamEvent(ev) => {
                let ev = *ev;
                let _ = ctx.stream_tx.send(ev);
            }
            Command::SetTrustMode(_) => {
                // trust_mode 更新由 engine.rs 的 select! 分支处理(拥有 &mut self)
            }
        }
    }

    if current_agent_input.is_some() {
        PendingCommandEffect::MessagesInjected {
            current_agent_input,
        }
    } else {
        PendingCommandEffect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_final_answer_empty_is_not_final() {
        // 空文本不视为完整回答。
        assert!(!looks_like_final_answer(""));
        assert!(!looks_like_final_answer("   "));
    }

    #[test]
    fn looks_like_final_answer_short_substantive_is_final() {
        // 短的非提问文本（任务完成的简短确认）同样视为最终回复。
        // 这是本次修复的核心：不再因长度不足把短回复送进总结阶段。
        assert!(looks_like_final_answer("好的，已完成。"));
        assert!(looks_like_final_answer(
            "已创建定时提醒：每个工作日 11:00 提醒你点外卖。"
        ));
    }

    #[test]
    fn looks_like_final_answer_long_substantive_is_final() {
        // 一段较长、不以问号结尾、不以提问短语开头的实质文本 → 视为完整回答。
        let text = "我重新检查了当前分支的全部改动，结论是核心问题已经修复。\
                    首先，AgentTurn 不再把整轮 elapsed_ms 当作深度思考耗时传给 ThinkingBlock，\
                    语义已经修正。其次，历史思考块固定 isActive 为 false，避免误计时与误展开。\
                    第三，showProcess 通过 useEffect 跟随 isActive 同步，完成后自动折叠过程。\
                    最后，summaryFrag 改为数组，多条非 react 助手回复不再互相覆盖。\
                    前端构建通过，整体改动合理，建议合并。";
        assert!(looks_like_final_answer(text));
    }

    #[test]
    fn looks_like_final_answer_ending_with_question_is_not_final() {
        // 以问号结尾 → 视为向用户提问，不作为最终回答（无论长短）。
        assert!(!looks_like_final_answer("请问需要我继续吗？"));
        assert!(!looks_like_final_answer(
            "我重新检查了代码，发现了一些问题，但还需要你确认以下几点？"
        ));
    }

    #[test]
    fn looks_like_final_answer_intro_question_phrase_is_not_final() {
        // 以提问短语开头 → 视为向用户提问，不作为最终回答（无论长短）。
        assert!(!looks_like_final_answer("请提供你的 API 凭据以便继续。"));
        assert!(!looks_like_final_answer("请确认以上配置是否正确。"));
        assert!(!looks_like_final_answer("你想使用哪种方案？"));
    }
}
