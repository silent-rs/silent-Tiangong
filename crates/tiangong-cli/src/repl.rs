//! CLI REPL — 类似 codex/claude code 风格交互

use anyhow::Result;
use tiangong_config::load_tiangong_config;
use tiangong_core::app_state::TiangongState;
use tiangong_core::core::TiangongCore;
use tiangong_types::{SessionStreamEvent, StreamEvent};

use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::commands;
use crate::completion;
use crate::input::InputReader;
use crate::output;

pub fn run(trust_mode: Option<tiangong_core::permission::TrustMode>) -> Result<()> {
    // 启动 Memory Actor（进程级单例，之后通过 tiangong_memory::global_handle() 访问）
    let workspace_id = std::env::current_dir()
        .ok()
        .map(|p| tiangong_memory::workspace_id_from_path(&p));
    match tiangong_memory::ensure_started(workspace_id) {
        Ok(_) => tracing::info!("Memory Actor 已启动"),
        Err(e) => tracing::warn!("Memory Actor 启动失败（非致命）: {}", e),
    }

    let mut state = TiangongState::load_or_default();
    let app_config = load_tiangong_config();
    let config = app_config.into_core_config_provider();
    let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();
    let core = TiangongCore::new(config.clone(), stream_tx);

    // CLI --trust-mode 参数覆盖
    if let Some(mode) = trust_mode {
        core.set_trust_mode(mode);
    }
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
            match commands::handle_command(&mut state, &config, trimmed, &mut draft_new_session) {
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
        handle_response(&stream_rx, &core);

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
fn handle_response(rx: &mpsc::Receiver<SessionStreamEvent>, core: &TiangongCore) {
    let mut state = ResponseState::new();
    let mut last_event_at = Instant::now();
    let timeout = Duration::from_secs(300);
    let poll_interval = Duration::from_millis(50);

    loop {
        let mut had_event = false;

        loop {
            match rx.try_recv() {
                Ok(session_event) => {
                    had_event = true;
                    last_event_at = Instant::now();
                    if state.process(&session_event.event, core) {
                        return;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    state.finish();
                    output::error("响应流已断开");
                    break;
                }
            }
        }

        if last_event_at.elapsed() >= timeout {
            state.finish();
            output::error("等待响应超时（300s）");
            break;
        }

        if !had_event {
            std::thread::sleep(poll_interval);
        }
    }
}

/// 响应流状态机
struct ResponseState {
    active_stream: ActiveStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveStream {
    Idle,
    Reasoning { message_id: String },
    Assistant { message_id: String },
    Worker { worker_id: String },
}

impl ResponseState {
    fn new() -> Self {
        Self {
            active_stream: ActiveStream::Idle,
        }
    }

    /// 处理单个事件，返回 true 表示本轮结束
    fn process(&mut self, event: &StreamEvent, core: &TiangongCore) -> bool {
        match event {
            StreamEvent::UserMessage { .. } => {
                // CLI 模式下用户消息由 REPL 自己显示，忽略
            }
            StreamEvent::Reasoning {
                message_id,
                content,
            } => {
                self.ensure_reasoning_stream(message_id);
                output::explanation_delta(content);
            }

            StreamEvent::Delta {
                message_id,
                content,
            } => {
                self.ensure_assistant_stream(message_id);
                output::delta(content);
            }

            StreamEvent::ToolCalls { names, .. } => {
                self.end_active_stream();
                output::tool_calls(names);
            }

            StreamEvent::ToolStart { name, args_summary } => {
                self.end_active_stream();
                if args_summary.is_empty() {
                    output::tool_start(name);
                } else {
                    output::tool_start(&format!("{name} {args_summary}"));
                }
            }

            StreamEvent::ToolResult { name, ok, output } => {
                self.end_active_stream();
                output::tool_result(name, *ok, output);
            }

            StreamEvent::ApprovalNeeded {
                request_id,
                tool_name,
                args_summary,
            } => {
                self.end_active_stream();
                output::approval_needed(tool_name, args_summary);
                // 等待用户输入 y/n
                let approved = loop {
                    eprint!("\x1b[1;33m  允许执行？(y/n): \x1b[0m");
                    let mut buf = String::new();
                    if std::io::stdin().read_line(&mut buf).is_err() {
                        break false;
                    }
                    match buf.trim().to_lowercase().as_str() {
                        "y" | "yes" => break true,
                        "n" | "no" => break false,
                        _ => {
                            eprintln!("  请输入 y 或 n");
                        }
                    }
                };
                core.respond_approval(request_id.clone(), approved);
                if approved {
                    output::status("已允许");
                } else {
                    output::warn("已拒绝");
                }
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

            StreamEvent::Retry {
                message,
                attempt,
                max_attempts,
            } => {
                self.end_active_stream();
                output::warn(&format!("重试 ({attempt}/{max_attempts})：{message}"));
            }

            StreamEvent::WorkerStarted {
                worker_id: _,
                worker_label,
            } => {
                self.end_active_stream();
                output::worker_started(worker_label);
            }

            StreamEvent::WorkerChunk {
                worker_id,
                worker_label,
                content,
            } => {
                self.ensure_worker_stream(worker_id, worker_label);
                output::worker_stream_delta(content);
            }

            StreamEvent::WorkerCompleted {
                worker_id: _,
                worker_label,
                success,
            } => {
                self.end_active_stream();
                output::worker_completed(worker_label, *success);
            }
        }
        false
    }

    fn ensure_reasoning_stream(&mut self, message_id: &str) {
        if !matches!(
            &self.active_stream,
            ActiveStream::Reasoning {
                message_id: current_message_id
            } if current_message_id == message_id
        ) {
            self.end_active_stream();
            output::explanation_start();
            self.active_stream = ActiveStream::Reasoning {
                message_id: message_id.to_string(),
            };
        }
    }

    fn ensure_assistant_stream(&mut self, message_id: &str) {
        if !matches!(
            &self.active_stream,
            ActiveStream::Assistant {
                message_id: current_message_id
            } if current_message_id == message_id
        ) {
            self.end_active_stream();
            output::assistant_start();
            self.active_stream = ActiveStream::Assistant {
                message_id: message_id.to_string(),
            };
        }
    }

    fn ensure_worker_stream(&mut self, worker_id: &str, worker_label: &str) {
        if !matches!(
            &self.active_stream,
            ActiveStream::Worker {
                worker_id: current_worker_id
            } if current_worker_id == worker_id
        ) {
            self.end_active_stream();
            output::worker_stream_start(worker_label);
            self.active_stream = ActiveStream::Worker {
                worker_id: worker_id.to_string(),
            };
        }
    }

    fn end_active_stream(&mut self) {
        match self.active_stream {
            ActiveStream::Idle => {}
            ActiveStream::Reasoning { .. } => output::explanation_end(),
            ActiveStream::Assistant { .. } => output::delta_end(),
            ActiveStream::Worker { .. } => output::worker_stream_end(),
        }
        self.active_stream = ActiveStream::Idle;
    }

    fn finish(&mut self) {
        self.end_active_stream();
        println!();
    }
}
