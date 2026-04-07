//! CLI REPL — 类似 codex/claude code 风格交互

use anyhow::Result;
use tiangong_core::core::TiangongCore;
use tiangong_types::StreamEvent;

use std::sync::mpsc;

use crate::completion;
use crate::input::InputReader;
use crate::output;

pub fn run() -> Result<()> {
    let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
    let core = TiangongCore::new(stream_tx);
    let mut reader = InputReader::new();

    output::welcome();

    loop {
        // 消费残留事件（上一轮可能有延迟到达的）
        while stream_rx.try_recv().is_ok() {}

        // 构建含 session ID 的 prompt
        let short_id: String = core.session_id().chars().take(8).collect();
        let prompt = format!("\x1b[2m{short_id}\x1b[0m \x1b[1;36m❯\x1b[0m ");

        let input = reader.read_line(&prompt, |buf, cursor| {
            if let Some((trigger, _start, prefix)) = completion::detect_trigger(buf, cursor) {
                let _ = (trigger, prefix);
                Vec::new()
            } else {
                Vec::new()
            }
        })?;

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
            handle_command(trimmed, &core);
            continue;
        }

        // 发送消息（用户输入已在 prompt 行显示，不重复打印）
        core.send_message(trimmed.to_string());

        // 处理响应流
        handle_response(&stream_rx);

        output::separator();
    }

    output::status("再见！");
    let _session = core.into_session();
    Ok(())
}

/// 处理 / 命令
fn handle_command(cmd: &str, core: &TiangongCore) {
    match cmd {
        "/help" | "/h" => {
            println!();
            output::status("可用命令：");
            output::status("  /help, /h     — 显示帮助");
            output::status("  /cancel, /c   — 取消当前执行");
            output::status("  /quit, /q     — 退出");
            println!();
        }
        "/cancel" | "/c" => {
            core.cancel();
            output::warn("已发送取消请求");
        }
        "/quit" | "/q" | "/exit" => {
            std::process::exit(0);
        }
        _ => {
            output::warn(&format!("未知命令：{cmd}，输入 /help 查看帮助"));
        }
    }
}

/// 处理完整的响应流
fn handle_response(rx: &mpsc::Receiver<StreamEvent>) {
    let mut state = ResponseState::new();

    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(300)) {
            Ok(event) => {
                let terminal = state.process(&event);
                if terminal {
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
    /// 是否已显示 thinking 指示
    thinking_shown: bool,
    /// thinking 缓冲
    reasoning_buf: String,
    /// 是否正在输出 assistant 文本
    in_delta: bool,
    /// 是否已输出过任何 delta
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
                // 首次 delta：清除 thinking 指示，输出 thinking 摘要
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
                // 结束当前 delta 流
                if self.in_delta {
                    output::delta_end();
                    self.in_delta = false;
                }
                // 清除 thinking 指示
                if self.thinking_shown && !self.has_delta {
                    output::thinking_clear();
                }
                // 输出 thinking 摘要
                if !self.reasoning_buf.is_empty() {
                    output::thinking_summary(&self.reasoning_buf);
                    self.reasoning_buf.clear();
                }
                // 显示工具调用
                output::tool_calls(names);
                // 重置状态（下一轮 LLM 可能有新的 delta）
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
        }
        false
    }

    /// 完成处理，清理状态
    fn finish(&mut self) {
        if self.in_delta {
            output::delta_end();
            self.in_delta = false;
        }
        // 如果 thinking 还在但没有 delta（如纯工具调用后直接 Done）
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
