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

/// 会话输出追踪状态
struct OutputTracker {
    last_msg_count: usize,
    last_assistant_content_len: usize,
    last_assistant_reasoning_len: usize,
    in_assistant_stream: bool,
    printed_header: bool,
    printed_reasoning: bool,
}

impl OutputTracker {
    fn new(msg_count: usize) -> Self {
        Self {
            last_msg_count: msg_count,
            last_assistant_content_len: 0,
            last_assistant_reasoning_len: 0,
            in_assistant_stream: false,
            printed_header: false,
            printed_reasoning: false,
        }
    }

    /// 重置追踪状态（新一轮对话开始）
    fn reset(&mut self, msg_count: usize) {
        self.last_msg_count = msg_count;
        self.last_assistant_content_len = 0;
        self.last_assistant_reasoning_len = 0;
        self.in_assistant_stream = false;
        self.printed_header = false;
        self.printed_reasoning = false;
    }

    /// 处理 session 消息变化，打印增量内容
    /// 返回 true 表示有新内容输出
    fn process_updates(&mut self, state: &TiangongState) -> bool {
        let mut had_output = false;

        let Some(session) = state.active_session() else {
            return false;
        };
        let msgs = &session.messages;

        // 处理新增的消息
        for msg in msgs.iter().skip(self.last_msg_count) {
            match msg.role {
                MessageRole::System => {
                    if self.in_assistant_stream {
                        output::flush_line();
                        self.in_assistant_stream = false;
                    }
                    if msg.content.starts_with("LLM 输出") {
                        output::print_llm_explanation(&msg.content);
                        had_output = true;
                    } else if msg.content.contains("tool_name:")
                        || msg.content.contains("exit_code")
                    {
                        output::print_tool_brief(&msg.content);
                        had_output = true;
                    }
                }
                MessageRole::Assistant => {
                    if !self.printed_header {
                        println!("{GREEN_BOLD}助手{RESET}");
                        self.printed_header = true;
                    }
                    // reasoning 在流式中不打印（终端无法覆盖），等完成后由 flush_remaining 打印完整摘要
                    self.last_assistant_reasoning_len = msg.reasoning_content.len();
                    // content
                    if !msg.content.is_empty() {
                        output::print_delta(&msg.content);
                        self.in_assistant_stream = true;
                        self.last_assistant_content_len = msg.content.len();
                        had_output = true;
                    }
                }
                _ => {}
            }
        }
        self.last_msg_count = msgs.len();

        // 检查最后一条 assistant 消息内容增长（流式追加）
        if let Some(last) = msgs.last()
            && last.role == MessageRole::Assistant
        {
            if last.reasoning_content.len() > self.last_assistant_reasoning_len {
                self.last_assistant_reasoning_len = last.reasoning_content.len();
            }
            if last.content.len() > self.last_assistant_content_len {
                if !self.printed_header {
                    println!("{GREEN_BOLD}助手{RESET}");
                    self.printed_header = true;
                }
                let delta = &last.content[self.last_assistant_content_len..];
                output::print_delta(delta);
                self.in_assistant_stream = true;
                self.last_assistant_content_len = last.content.len();
                had_output = true;
            }
        }

        had_output
    }

    /// 任务完成后确保所有内容已输出
    fn flush_remaining(&mut self, state: &TiangongState) {
        // 最后一次 poll + 处理
        self.process_updates(state);

        // 检查 assistant 消息是否还有未输出的内容
        if let Some(session) = state.active_session()
            && let Some(last) = session
                .messages
                .iter()
                .rev()
                .find(|m| m.role == MessageRole::Assistant)
        {
            if !self.printed_header {
                println!("{GREEN_BOLD}助手{RESET}");
                self.printed_header = true;
            }
            // 如果流式中正在输出 content，先换行再打印 reasoning
            if self.in_assistant_stream {
                output::flush_line();
                self.in_assistant_stream = false;
            }
            // 打印完整 reasoning 摘要（流式中没打印过）
            if !self.printed_reasoning {
                let reasoning = last.reasoning_content.trim();
                if !reasoning.is_empty() {
                    let summary: String = if reasoning.chars().count() > 60 {
                        let truncated: String = reasoning.chars().take(57).collect();
                        format!("{truncated}...")
                    } else {
                        reasoning.to_string()
                    };
                    println!("  {DIM}[思考] {summary}{RESET}");
                    self.printed_reasoning = true;
                }
            }
            // 打印未输出的 content
            if last.content.len() > self.last_assistant_content_len {
                let delta = &last.content[self.last_assistant_content_len..];
                output::print_delta(delta);
                self.in_assistant_stream = true;
            }
        }

        if self.in_assistant_stream {
            output::flush_line();
            self.in_assistant_stream = false;
        }

        // 兜底：完全没有流式输出
        if !self.printed_header {
            let snapshot = state.run_snapshot();
            if matches!(snapshot.status, RunStatus::Completed | RunStatus::Idle)
                && let Some(session) = state.active_session()
                && let Some(last) = session.messages.last()
                && last.role == MessageRole::Assistant
            {
                output::print_assistant_message(last);
            }
        }

        // 错误处理
        let snapshot = state.run_snapshot();
        if matches!(snapshot.status, RunStatus::Failed) {
            let err_msg = snapshot.last_error.as_deref().unwrap_or("执行失败");
            output::print_error(err_msg);
        }
    }
}

pub fn run() -> Result<()> {
    let mut state = TiangongState::load_or_default();
    let mut reader = InputReader::new();
    let mut draft_new_session = true;
    let mut tracker = OutputTracker::new(
        state
            .active_session()
            .map(|s| s.messages.len())
            .unwrap_or(0),
    );

    output::print_status("天工 CLI — /help 查看命令，Ctrl+C 清空/退出");

    loop {
        // 如果有活跃任务，持续 poll 直到完成或用户有新输入
        if state.has_pending_turn() {
            state.poll_pending_turn();
            tracker.process_updates(&state);

            if !state.has_pending_turn() {
                // 任务刚完成：flush 剩余内容
                tracker.flush_remaining(&state);
                println!();
                // 重置后等待下一个输入
                state.report_run_idle(format!(
                    "模型供应商：{}",
                    state.provider_label()
                ));
            }

            // 短暂等待后继续循环
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        // 无活跃任务：显示 prompt 等待用户输入
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

        // 重置追踪（新一轮对话开始，从当前消息数开始追踪）
        tracker.reset(
            state
                .active_session()
                .map(|s| s.messages.len())
                .unwrap_or(0),
        );

        // 不 break，回到循环顶部由 poll 分支处理
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
