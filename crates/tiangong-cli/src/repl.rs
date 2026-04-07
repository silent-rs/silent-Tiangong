use anyhow::Result;
use tiangong_core::core::TiangongCore;
use tiangong_types::StreamEvent;

use std::sync::mpsc;

use crate::completion;
use crate::input::InputReader;
use crate::output;

const PROMPT: &str = "\x1b[1;36m› \x1b[0m";
const DIM: &str = "\x1b[2m";
const GREEN_BOLD: &str = "\x1b[1;32m";
const RESET: &str = "\x1b[0m";

pub fn run() -> Result<()> {
    let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
    let core = TiangongCore::new(stream_tx);
    let mut reader = InputReader::new();
    let mut printed_header = false;
    let mut in_stream = false;
    let mut reasoning_buf = String::new();

    output::print_status("天工 CLI — /help 查看命令，Ctrl+C 清空/退出");

    loop {
        // 消费待处理的 StreamEvent（非阻塞）
        while let Ok(event) = stream_rx.try_recv() {
            let is_terminal = matches!(event, StreamEvent::Done | StreamEvent::Error { .. });
            process_event(&event, &mut printed_header, &mut in_stream, &mut reasoning_buf);
            if is_terminal {
                if in_stream { output::flush_line(); in_stream = false; }
                printed_header = false;
                reasoning_buf.clear();
                println!();
            }
        }

        // 显示 prompt 等待输入
        print_separator();
        print_status_line(&core);

        let input = reader.read_line(PROMPT, |buf, cursor| {
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
        print_separator();

        // / 命令
        if trimmed.starts_with('/') {
            match trimmed {
                "/help" | "/h" => {
                    output::print_status("命令：/help /cancel /quit");
                    continue;
                }
                "/cancel" | "/c" => {
                    core.cancel();
                    output::print_status("已发送取消请求");
                    continue;
                }
                "/quit" | "/q" | "/exit" => break,
                _ => {
                    output::print_warn(&format!("未知命令：{trimmed}"));
                    continue;
                }
            }
        }

        // 发送消息
        output::print_user_message(trimmed);
        core.send_message(trimmed.to_string());

        // 重置输出状态
        printed_header = false;
        in_stream = false;
        reasoning_buf.clear();

        // 阻塞等待本轮完成
        loop {
            match stream_rx.recv_timeout(std::time::Duration::from_secs(300)) {
                Ok(event) => {
                    let is_terminal = matches!(event, StreamEvent::Done | StreamEvent::Error { .. });
                    process_event(&event, &mut printed_header, &mut in_stream, &mut reasoning_buf);
                    if is_terminal {
                        if in_stream { output::flush_line(); in_stream = false; }
                        if !reasoning_buf.is_empty() { flush_reasoning(&reasoning_buf); }
                        printed_header = false;
                        reasoning_buf.clear();
                        println!();
                        break;
                    }
                }
                Err(_) => {
                    output::print_error("等待响应超时（300s）");
                    break;
                }
            }
        }
    }

    output::print_status("再见！");
    let _session = core.into_session();
    Ok(())
}

fn process_event(
    event: &StreamEvent,
    printed_header: &mut bool,
    in_stream: &mut bool,
    reasoning_buf: &mut String,
) {
    match event {
        StreamEvent::Delta { content } => {
            if !*printed_header {
                flush_reasoning(reasoning_buf);
                reasoning_buf.clear();
                println!("{GREEN_BOLD}助手{RESET}");
                *printed_header = true;
            }
            output::print_delta(content);
            *in_stream = true;
        }
        StreamEvent::Reasoning { content } => {
            reasoning_buf.push_str(content);
        }
        StreamEvent::ToolStart { name, .. } => {
            if *in_stream { output::flush_line(); *in_stream = false; }
            output::print_status(&format!("  ⚙ 执行 {name}..."));
        }
        StreamEvent::ToolResult { name, ok, output } => {
            let status = if *ok { "✓" } else { "✗" };
            let preview: String = output.lines().next().unwrap_or("").chars().take(80).collect();
            output::print_status(&format!("  {status} {name}: {preview}"));
        }
        StreamEvent::ToolCalls { names } => {
            if *in_stream { output::flush_line(); *in_stream = false; }
            flush_reasoning(reasoning_buf);
            reasoning_buf.clear();
            output::print_status(&format!("  {DIM}[调用工具: {}]{RESET}", names.join(", ")));
        }
        StreamEvent::ApprovalNeeded { tool_name, args_summary, .. } => {
            output::print_warn(&format!("工具 {tool_name} 需要审批：{args_summary}"));
        }
        StreamEvent::Done | StreamEvent::Error { .. } => {}
    }
}

fn flush_reasoning(buf: &str) {
    let trimmed = buf.trim();
    if trimmed.is_empty() { return; }
    let summary: String = if trimmed.chars().count() > 80 {
        let truncated: String = trimmed.chars().take(77).collect();
        format!("{truncated}...")
    } else {
        trimmed.to_string()
    };
    println!("  {DIM}[思考] {summary}{RESET}");
}

fn print_separator() {
    let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
    println!("{DIM}{}{RESET}", "─".repeat(width));
}

fn print_status_line(core: &TiangongCore) {
    let short_id: String = core.session_id().chars().take(8).collect();
    println!("{DIM}[{short_id}]{RESET}");
}
