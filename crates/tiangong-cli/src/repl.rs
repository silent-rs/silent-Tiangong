//! CLI REPL — 类似 codex/claude code 风格交互

use anyhow::Result;
use tiangong_core::app_state::TiangongState;
use tiangong_core::core::TiangongCore;
use tiangong_types::StreamEvent;

use std::sync::mpsc;

use crate::commands;
use crate::completion;
use crate::input::InputReader;
use crate::output;

pub fn run() -> Result<()> {
    let mut state = TiangongState::load_or_default();
    let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
    let core = TiangongCore::new(stream_tx);
    let mut reader = InputReader::new();
    let mut draft_new_session = true;

    output::welcome();

    loop {
        // 消费残留事件
        while stream_rx.try_recv().is_ok() {}

        // prompt
        let short_id: String = core.session_id().chars().take(8).collect();
        let prompt = format!("\x1b[2m{short_id}\x1b[0m \x1b[1;36m❯\x1b[0m ");

        let input = {
            let state_ref = &state;
            reader.read_line(&prompt, |buf, cursor| {
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

        // / 命令（通过 TiangongState 处理）
        if trimmed.starts_with('/') {
            match commands::handle_command(&mut state, trimmed, &mut draft_new_session) {
                Ok(true) => break,
                Ok(false) => continue,
                Err(err) => {
                    output::error(&format!("{err}"));
                    continue;
                }
            }
        }

        // 首次发送时创建会话
        if draft_new_session {
            state.create_session();
            draft_new_session = false;
        }

        // 发送消息
        core.send_message(trimmed.to_string());

        // 处理响应流
        handle_response(&stream_rx);

        output::separator();
    }

    output::status("再见！");
    // 获取 Core 的最终 session 并持久化
    let final_session = core.into_session();
    if !final_session.messages.is_empty() {
        state.save_core_session(final_session);
    }
    Ok(())
}

/// 处理完整的响应流
fn handle_response(rx: &mpsc::Receiver<StreamEvent>) {
    let mut state = ResponseState::new();

    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(300)) {
            Ok(event) => {
                if state.process(&event) {
                    break;
                }
            }
            Err(_) => {
                state.finish();
                output::error("等待响应超时（300s）");
                break;
            }
        }
    }
}

/// 响应流状态机
struct ResponseState {
    thinking_shown: bool,
    reasoning_buf: String,
    in_delta: bool,
    has_delta: bool,
}

impl ResponseState {
    fn new() -> Self {
        Self {
            thinking_shown: false,
            reasoning_buf: String::new(),
            in_delta: false,
            has_delta: false,
        }
    }

    /// 处理单个事件，返回 true 表示本轮结束
    fn process(&mut self, event: &StreamEvent) -> bool {
        match event {
            StreamEvent::Reasoning { content } => {
                if !self.thinking_shown {
                    output::thinking_start();
                    self.thinking_shown = true;
                }
                self.reasoning_buf.push_str(content);
            }

            StreamEvent::Delta { content } => {
                if !self.has_delta {
                    if self.thinking_shown {
                        output::thinking_clear();
                    }
                    if !self.reasoning_buf.is_empty() {
                        output::thinking_summary(&self.reasoning_buf);
                    }
                    output::assistant_start();
                    self.has_delta = true;
                }
                output::delta(content);
                self.in_delta = true;
            }

            StreamEvent::ToolCalls { names, .. } => {
                if self.in_delta {
                    output::delta_end();
                    self.in_delta = false;
                }
                if self.thinking_shown && !self.has_delta {
                    output::thinking_clear();
                }
                if !self.reasoning_buf.is_empty() {
                    output::thinking_summary(&self.reasoning_buf);
                    self.reasoning_buf.clear();
                }
                output::tool_calls(names);
                self.thinking_shown = false;
                self.has_delta = false;
            }

            StreamEvent::ToolStart { name, .. } => {
                output::tool_start(name);
            }

            StreamEvent::ToolResult { name, ok, output } => {
                output::tool_result(name, *ok, output);
            }

            StreamEvent::ApprovalNeeded {
                tool_name,
                args_summary,
                ..
            } => {
                output::approval_needed(tool_name, args_summary);
            }

            StreamEvent::Done { .. } => {
                self.finish();
                return true;
            }

            StreamEvent::Error { message } => {
                self.finish();
                output::error(message);
                return true;
            }

            StreamEvent::WorkerStarted {
                worker_id: _,
                worker_label,
            } => {
                output::worker_started(worker_label);
            }

            StreamEvent::WorkerChunk {
                worker_id: _,
                worker_label: _,
                content,
            } => {
                output::delta(content);
                self.in_delta = true;
            }

            StreamEvent::WorkerCompleted {
                worker_id: _,
                worker_label,
                success,
            } => {
                output::worker_completed(worker_label, *success);
            }
        }
        false
    }

    fn finish(&mut self) {
        if self.in_delta {
            output::delta_end();
            self.in_delta = false;
        }
        if self.thinking_shown && !self.has_delta {
            output::thinking_clear();
        }
        if !self.reasoning_buf.is_empty() && !self.has_delta {
            output::thinking_summary(&self.reasoning_buf);
        }
        self.reasoning_buf.clear();
        println!();
    }
}
