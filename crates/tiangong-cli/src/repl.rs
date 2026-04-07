use anyhow::Result;
use tiangong_core::app_state::{StreamEvent, TiangongState};
use tiangong_core::runtime::RunStatus;

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
    let mut draft_new_session = true;
    let mut printed_header = false;
    let mut in_stream = false;
    let mut reasoning_buf = String::new();

    output::print_status("天工 CLI — /help 查看命令，Ctrl+C 清空/退出");

    loop {
        // 有活跃任务：消费事件流展示内容
        if state.has_pending_turn() {
            let events = state.poll_events();

            for event in events {
                match event {
                    StreamEvent::Delta { content: text } => {
                        if !printed_header {
                            // 先输出缓积的 reasoning
                            flush_reasoning(&reasoning_buf);
                            println!("{GREEN_BOLD}助手{RESET}");
                            printed_header = true;
                        }
                        output::print_delta(&text);
                        in_stream = true;
                    }
                    StreamEvent::Reasoning { content: text } => {
                        reasoning_buf.push_str(&text);
                    }
                    StreamEvent::ToolStart { name, .. } => {
                        if in_stream {
                            output::flush_line();
                            in_stream = false;
                        }
                        output::print_status(&format!("  ⚙ 执行 {name}..."));
                    }
                    StreamEvent::ToolResult { name, ok, output: out } => {
                        let status = if ok { "✓" } else { "✗" };
                        let preview: String = out.lines().next().unwrap_or("").chars().take(80).collect();
                        output::print_status(&format!("  {status} {name}: {preview}"));
                    }
                    StreamEvent::ToolCalls { names } => {
                        if in_stream {
                            output::flush_line();
                            in_stream = false;
                        }
                        // 输出缓积的 reasoning（工具调用前的思考）
                        flush_reasoning(&reasoning_buf);
                        reasoning_buf.clear();
                        output::print_status(&format!(
                            "  {DIM}[调用工具: {}]{RESET}",
                            names.join(", ")
                        ));
                    }
                    StreamEvent::Done => {
                        if in_stream {
                            output::flush_line();
                            in_stream = false;
                        }
                        // 最终输出完整 reasoning 摘要（如果还没输出过）
                        if !reasoning_buf.is_empty() && !printed_header {
                            flush_reasoning(&reasoning_buf);
                        }
                        reasoning_buf.clear();

                        // 重置状态
                        state.report_run_idle(format!(
                            "模型供应商：{}",
                            state.provider_label()
                        ));
                        printed_header = false;
                        println!();
                    }
                    StreamEvent::Error { message: err } => {
                        if in_stream {
                            output::flush_line();
                            in_stream = false;
                        }
                        output::print_error(&err);
                        printed_header = false;
                        reasoning_buf.clear();
                        println!();
                    }
                    StreamEvent::ApprovalNeeded {
                        tool_name,
                        args_summary,
                        ..
                    } => {
                        output::print_warn(&format!(
                            "工具 {tool_name} 需要审批：{args_summary}"
                        ));
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        // 无活跃任务：显示 prompt 等待输入
        print_separator();
        print_status_line(&state, draft_new_session);

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

        let input = match input {
            Some(line) => line,
            None => break,
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        reader.push_history(trimmed);
        print_separator();

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

        if draft_new_session {
            state.create_session();
            draft_new_session = false;
        }

        output::print_user_message(trimmed);
        state.update_draft(trimmed.to_string());

        if let Err(err) = state.send_current_input() {
            output::print_error(&format!("发送失败：{err}"));
            continue;
        }

        // 重置输出状态
        printed_header = false;
        in_stream = false;
        reasoning_buf.clear();
    }

    output::print_status("再见！");
    Ok(())
}

fn flush_reasoning(buf: &str) {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return;
    }
    let summary: String = if trimmed.chars().count() > 80 {
        let truncated: String = trimmed.chars().take(77).collect();
        format!("{truncated}...")
    } else {
        trimmed.to_string()
    };
    println!("  {DIM}[思考] {summary}{RESET}");
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
