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

        // 等待并轮询结果
        output::print_status("正在请求...");
        loop {
            state.poll_pending_turn();
            if !state.has_pending_turn() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // 清除 "正在请求..." 行
        print!("\x1b[A\x1b[2K");

        // 打印结果
        let snapshot = state.run_snapshot();
        match snapshot.status {
            RunStatus::Completed | RunStatus::Idle => {
                if let Some(session) = state.active_session()
                    && let Some(last) = session.messages.last()
                    && last.role == MessageRole::Assistant
                {
                    output::print_assistant_message(last);
                }
            }
            RunStatus::Failed => {
                let err_msg = snapshot.last_error.as_deref().unwrap_or("执行失败");
                output::print_error(err_msg);
            }
            _ => {}
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
        RunStatus::Completed => "done",
        RunStatus::Failed => "failed",
    };
    println!("{DIM}[{status}] {session_title} | {model}{RESET}");
}
