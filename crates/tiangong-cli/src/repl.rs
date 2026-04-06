use anyhow::Result;
use tiangong_core::app_state::TiangongState;
use tiangong_core::runtime::RunStatus;
use tiangong_core::session::MessageRole;

use crate::commands;
use crate::completion;
use crate::input::InputReader;
use crate::output;

const PROMPT: &str = "\x1b[1;36m› \x1b[0m";
const DIM: &str = "\x1b[2m";
const GREEN_BOLD: &str = "\x1b[1;32m";
const RESET: &str = "\x1b[0m";

pub fn run() -> Result<()> {
    let mut state = TiangongState::load_or_default();
    let mut reader = InputReader::new();

    // 标记为草稿新会话，首次发送消息时才真正创建
    let mut draft_new_session = true;

    // 打印欢迎
    output::print_status("天工 CLI — /help 查看命令，Ctrl+C 清空/退出");

    loop {
        print_separator();

        let input = {
            let state_ref = &state;
            reader.read_line(PROMPT, |buf, cursor| {
                if let Some((trigger, _start, prefix)) = completion::detect_trigger(buf, cursor) {
                    completion::complete(trigger, &prefix, state_ref)
                } else {
                    Vec::new()
                }
            })?
        };

        print_separator();
        print_status_line(&state, draft_new_session);

        let input = match input {
            Some(line) => line,
            None => break,
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        reader.push_history(trimmed);

        // / 命令
        if trimmed.starts_with('/') {
            match commands::handle_command(&mut state, trimmed, &mut draft_new_session) {
                Ok(true) => break,
                Ok(false) => continue,
                Err(err) => {
                    output::print_error(&format!("命令执行失败：{err}"));
                    continue;
                }
            }
        }

        // 检查是否有正在进行的任务
        if state.has_pending_turn() {
            output::print_warn("当前请求进行中，请等待完成后再发送");
            continue;
        }

        // 首次发送时才真正创建会话
        if draft_new_session {
            state.create_session();
            draft_new_session = false;
        }

        // 发送对话
        output::print_user_message(trimmed);
        state.update_draft(trimmed.to_string());

        if let Err(err) = state.send_current_input() {
            output::print_error(&format!("发送失败：{err}"));
            continue;
        }

        // 实时流式轮询：边执行边展示中间状态
        {
            let mut last_msg_count = state
                .active_session()
                .map(|s| s.messages.len())
                .unwrap_or(0);
            let mut last_assistant_content_len: usize = 0;
            let mut last_assistant_reasoning_len: usize = 0;
            let mut in_assistant_stream = false;
            let mut printed_header = false;

            loop {
                state.poll_pending_turn();

                if let Some(session) = state.active_session() {
                    let msgs = &session.messages;

                    // 处理新增的消息
                    for msg in msgs.iter().skip(last_msg_count) {
                        match msg.role {
                            MessageRole::System => {
                                // 如果之前在流式输出 assistant，先换行
                                if in_assistant_stream {
                                    output::flush_line();
                                    in_assistant_stream = false;
                                }
                                if msg.content.starts_with("LLM 输出") {
                                    output::print_llm_explanation(&msg.content);
                                } else if msg.content.contains("tool_name:")
                                    || msg.content.contains("exit_code")
                                {
                                    output::print_tool_brief(&msg.content);
                                }
                                // 其他系统消息静默跳过
                            }
                            MessageRole::Assistant => {
                                // 标记进入 assistant 流式输出
                                if !printed_header {
                                    println!("{GREEN_BOLD}助手{RESET}");
                                    printed_header = true;
                                }
                                // reasoning
                                let reasoning = msg.reasoning_content.trim();
                                if !reasoning.is_empty() {
                                    let summary: String = if reasoning.chars().count() > 60 {
                                        let truncated: String = reasoning.chars().take(57).collect();
                                        format!("{truncated}...")
                                    } else {
                                        reasoning.to_string()
                                    };
                                    println!("  {DIM}[思考] {summary}{RESET}");
                                }
                                // content
                                if !msg.content.is_empty() {
                                    output::print_delta(&msg.content);
                                    in_assistant_stream = true;
                                    last_assistant_content_len = msg.content.len();
                                    last_assistant_reasoning_len = msg.reasoning_content.len();
                                }
                            }
                            _ => {}
                        }
                    }
                    last_msg_count = msgs.len();

                    // 检查最后一条 assistant 消息内容增长（流式追加）
                    if let Some(last) = msgs.last() {
                        if last.role == MessageRole::Assistant {
                            // reasoning 增量
                            if last.reasoning_content.len() > last_assistant_reasoning_len {
                                // reasoning 变化时仅更新追踪长度（不重复打印摘要）
                                last_assistant_reasoning_len = last.reasoning_content.len();
                            }
                            // content 增量
                            if last.content.len() > last_assistant_content_len {
                                if !printed_header {
                                    println!("{GREEN_BOLD}助手{RESET}");
                                    printed_header = true;
                                }
                                let delta = &last.content[last_assistant_content_len..];
                                output::print_delta(delta);
                                in_assistant_stream = true;
                                last_assistant_content_len = last.content.len();
                            }
                        }
                        // 检查最后一条系统消息内容增长（流式 thinking）
                        if last.role == MessageRole::System
                            && last.content.starts_with("LLM 输出")
                            && !last.content.contains("\ntokens:")
                        {
                            // 流式阶段的系统消息，内容在增长
                            // 不做增量打印 — 该消息完成后会作为新消息被处理
                        }
                    }
                }

                if !state.has_pending_turn() {
                    // 退出前最后一次 poll，确保所有事件已消费
                    state.poll_pending_turn();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            // 退出后打印 assistant 消息中未显示的剩余内容
            if let Some(session) = state.active_session()
                && let Some(last) = session.messages.iter().rev().find(|m| m.role == MessageRole::Assistant)
            {
                    // 打印未显示的 reasoning
                    if !printed_header && !last.reasoning_content.trim().is_empty() {
                        println!("{GREEN_BOLD}助手{RESET}");
                        printed_header = true;
                        let reasoning = last.reasoning_content.trim();
                        let summary: String = if reasoning.chars().count() > 60 {
                            let truncated: String = reasoning.chars().take(57).collect();
                            format!("{truncated}...")
                        } else {
                            reasoning.to_string()
                        };
                        println!("  {DIM}[思考] {summary}{RESET}");
                    }
                    // 打印未显示的 content
                    if last.content.len() > last_assistant_content_len {
                        if !printed_header {
                            println!("{GREEN_BOLD}助手{RESET}");
                            printed_header = true;
                        }
                        let delta = &last.content[last_assistant_content_len..];
                        output::print_delta(delta);
                        in_assistant_stream = true;
                    }
            }

            // 确保最后换行
            if in_assistant_stream {
                output::flush_line();
            }

            // 处理失败情况
            let snapshot = state.run_snapshot();
            if matches!(snapshot.status, RunStatus::Failed) {
                let err_msg = snapshot.last_error.as_deref().unwrap_or("执行失败");
                output::print_error(err_msg);
            }

            // 如果完全没有流式输出（非流式模式兜底）
            if !printed_header {
                match snapshot.status {
                    RunStatus::Completed | RunStatus::Idle => {
                        if let Some(session) = state.active_session()
                            && let Some(last) = session.messages.last()
                            && last.role == MessageRole::Assistant
                        {
                            output::print_assistant_message(last);
                        }
                    }
                    _ => {}
                }
            }

            println!();
        }
    }

    output::print_status("再见！");
    Ok(())
}

fn print_separator() {
    let width = crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80);
    println!("{DIM}{}{RESET}", "─".repeat(width));
}

fn print_status_line(state: &TiangongState, draft_new: bool) {
    let model = state.current_model();
    let session_title = if draft_new {
        "新对话"
    } else {
        state
            .active_session()
            .map(|s| s.title.as_str())
            .unwrap_or("无会话")
    };
    let run_status = state.run_snapshot().status;
    let status = match run_status {
        RunStatus::Idle => "idle",
        RunStatus::Planning => "planning",
        RunStatus::Executing => "executing",
        RunStatus::WaitingApproval => "waiting_approval",
        RunStatus::Completed => "done",
        RunStatus::Failed => "failed",
    };
    println!("{DIM}[{status}] {session_title} | {model}{RESET}");
}
