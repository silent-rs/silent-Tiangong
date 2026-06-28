//! ReAct 主循环的通用辅助函数。
//!
//! 这些函数不依赖 `ReactEngine` 自身状态（无 `&self`），而是操作传入的
//! `Session` / `RuntimeEngine` / 命令通道，属于与主状态机解耦的纯过程性逻辑：
//! - 命令排空（`drain_pending_commands_async`）
//! - 浏览器页面自动观察注入（`maybe_inject_browser_update`）
//! - 非阻塞取消检查（`check_cancel`）
//! - 最终回答启发式判断（`looks_like_final_answer`）

use std::sync::mpsc::Sender as StdSender;

use tokio::sync::mpsc as tokio_mpsc;

use crate::core::command::{Command, PendingCommandEffect};
use crate::react::message::append_or_reuse_user_message;
use crate::runtime::RuntimeEngine;
use crate::session::Session;
use tiangong_types::StreamEvent;

/// ReAct 阶段给出一段实质性回答时，可跳过总结阶段直接作为最终回复。
pub(super) const FINAL_ANSWER_MIN_CHARS: usize = 200;

/// 判断 ReAct 阶段的文本回复是否「看起来像一个完整回答」（而非向用户提问）。
///
/// 用于智能提升：当本轮执行过工具、但模型已给出一段足够长的实质文本，
/// 且不像是反问用户（以问号结尾或显式请求输入）时，直接把它作为最终回复，
/// 避免总结阶段再生成一个更精简、反而丢失细节的版本。
pub(super) fn looks_like_final_answer(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() < FINAL_ANSWER_MIN_CHARS {
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
    engine: &RuntimeEngine,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> PendingCommandEffect {
    let mut injected_message = false;

    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Cancel => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return PendingCommandEffect::Terminate;
            }
            Command::CancelAgent { .. } => {}
            Command::Shutdown => return PendingCommandEffect::Terminate,
            Command::Message {
                content,
                message_id,
                media,
            } => {
                let mid = append_or_reuse_user_message(session, &content, message_id, media);
                let msg_media = session
                    .messages
                    .iter()
                    .find(|message| message.id == mid)
                    .map(|message| message.media.clone())
                    .unwrap_or_default();
                let _ = stream_tx.send(StreamEvent::UserMessage {
                    message_id: mid,
                    content: content.clone(),
                    media: msg_media,
                });
                injected_message = true;
            }
            Command::UpdateCwd { cwd } => {
                session.cwd = cwd;
                crate::core::apply_session_cwd(session);
            }
            Command::ReloadConfig => {}
            Command::Approval { .. } => {}
            Command::InjectTool { tool_name, payload } => {
                crate::react::message::inject_tool_to_session(
                    session, stream_tx, &tool_name, &payload,
                );
                injected_message = true;
            }
            Command::CompressContext => {
                crate::core::compress_context_for_session(session, engine, stream_tx);
            }
            Command::ResetContext => {
                crate::core::reset_context_for_session(session, stream_tx, engine);
            }
        }
    }

    if injected_message {
        PendingCommandEffect::MessageInjected
    } else {
        PendingCommandEffect::None
    }
}

/// 节流（≥5s）地观察浏览器当前页面，发生变化时把快照注入会话上下文。
pub(super) async fn maybe_inject_browser_update(
    engine: &RuntimeEngine,
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    last_snapshot: &mut Option<crate::browser_trait::PageSnapshot>,
    last_check: &mut Option<std::time::Instant>,
    force_check: bool,
) {
    let fetcher = match engine.page_fetcher() {
        Some(f) => f,
        None => {
            tracing::debug!(
                session_id = %session.id,
                force_check,
                "skip browser auto observe: no page fetcher"
            );
            return;
        }
    };
    // 首次检测无间隔限制；后续检测至少间隔 5 秒，避免频繁 observe_page 拖慢执行
    let now = std::time::Instant::now();
    if last_snapshot.is_some() && !force_check {
        let min_interval = std::time::Duration::from_secs(5);
        if let Some(prev) = last_check
            && now.duration_since(*prev) < min_interval
        {
            tracing::debug!(
                session_id = %session.id,
                elapsed_ms = now.duration_since(*prev).as_millis() as u64,
                "skip browser auto observe: throttled"
            );
            return;
        }
    }
    *last_check = Some(now);
    let snapshot =
        match tokio::time::timeout(std::time::Duration::from_secs(3), fetcher.observe_page()).await
        {
            Ok(Some(s)) => s,
            Ok(None) => {
                tracing::debug!(
                    session_id = %session.id,
                    "skip browser auto observe: no snapshot"
                );
                return;
            }
            Err(_) => {
                tracing::warn!(
                    session_id = %session.id,
                    "browser auto observe timeout"
                );
                return;
            }
        };
    if snapshot.url.is_empty() {
        tracing::debug!(
            session_id = %session.id,
            "skip browser auto observe: empty url"
        );
        return;
    }

    let feedback = crate::browser_trait::format_browser_events(&snapshot.events);
    let has_feedback = feedback.is_some();
    tracing::debug!(
        session_id = %session.id,
        url = %snapshot.url,
        title = %snapshot.title,
        text_len = snapshot.text.len(),
        events_len = snapshot.events.len(),
        has_feedback,
        force_check,
        "browser auto observe snapshot"
    );
    let should_inject = match last_snapshot {
        None => true,
        Some(prev) => {
            has_feedback
                || prev.url != snapshot.url
                || (snapshot.text.len() as i64 - prev.text.len() as i64).unsigned_abs() > 500
        }
    };
    if !should_inject {
        tracing::debug!(
            session_id = %session.id,
            url = %snapshot.url,
            text_len = snapshot.text.len(),
            "skip browser auto inject: unchanged"
        );
        *last_snapshot = Some(snapshot);
        return;
    }

    let tabs: Vec<(String, String, String)> = snapshot
        .tabs
        .iter()
        .map(|t| (t.id.clone(), t.url.clone(), t.title.clone()))
        .collect();
    crate::react::message::inject_tool_to_session(
        session,
        stream_tx,
        "browser_data",
        &serde_json::json!({
            "title": snapshot.title,
            "url": snapshot.url,
            "text": snapshot.text,
            "tabs": tabs,
            "active_tab_id": snapshot.active_tab_id,
            "feedback": feedback,
        }),
    );
    tracing::info!(
        session_id = %session.id,
        url = %snapshot.url,
        text_len = snapshot.text.len(),
        events_len = snapshot.events.len(),
        has_feedback,
        "browser auto content injected"
    );
    *last_snapshot = Some(snapshot);
}

/// 非阻塞检查是否有取消或关闭命令待处理。
pub(super) fn check_cancel(
    session: &mut Session,
    engine: &RuntimeEngine,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    stream_tx: &StdSender<StreamEvent>,
) -> bool {
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Cancel | Command::Shutdown => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return true;
            }
            Command::CancelAgent { .. } => {}
            Command::CompressContext => {
                crate::core::compress_context_for_session(session, engine, stream_tx);
            }
            Command::ResetContext => {
                crate::core::reset_context_for_session(session, stream_tx, engine);
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_final_answer_short_text_is_not_final() {
        // 过短文本不视为完整回答（即便执行过工具）。
        assert!(!looks_like_final_answer("好的，已完成。"));
    }

    #[test]
    fn looks_like_final_answer_long_substantive_is_final() {
        // 一段足够长、不以问号结尾、不以提问短语开头的实质文本 → 视为完整回答。
        let text = "我重新检查了当前分支的全部改动，结论是核心问题已经修复。\
                    首先，AgentTurn 不再把整轮 elapsed_ms 当作深度思考耗时传给 ThinkingBlock，\
                    语义已经修正。其次，历史思考块固定 isActive 为 false，避免误计时与误展开。\
                    第三，showProcess 通过 useEffect 跟随 isActive 同步，完成后自动折叠过程。\
                    最后，summaryFrag 改为数组，多条非 react 助手回复不再互相覆盖。\
                    前端构建通过，整体改动合理，建议合并。";
        assert!(text.chars().count() >= FINAL_ANSWER_MIN_CHARS);
        assert!(looks_like_final_answer(text));
    }

    #[test]
    fn looks_like_final_answer_ending_with_question_is_not_final() {
        let mut text = "我重新检查了代码，发现了一些问题，但还需要你确认以下几点：".to_string();
        // 补足长度后仍以问号结尾 → 视为向用户提问，不作为最终回答。
        while text.chars().count() < FINAL_ANSWER_MIN_CHARS + 50 {
            text.push_str("补充说明内容。");
        }
        text.push('？');
        assert!(!looks_like_final_answer(&text));
    }

    #[test]
    fn looks_like_final_answer_intro_question_phrase_is_not_final() {
        let mut text = "请提供你的 API 凭据以便继续：".to_string();
        while text.chars().count() < FINAL_ANSWER_MIN_CHARS + 50 {
            text.push_str("这里需要更多信息。");
        }
        assert!(!looks_like_final_answer(&text));
    }
}
