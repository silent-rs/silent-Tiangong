use tiangong_core::session::{Message, MessageRole};

// ANSI 颜色码
const CYAN_BOLD: &str = "\x1b[1;36m";
const GREEN_BOLD: &str = "\x1b[1;32m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// 打印单条用户消息
pub fn print_user_message(content: &str) {
    println!("{CYAN_BOLD}你{RESET}");
    for line in content.lines() {
        println!("  {line}");
    }
    println!();
}

/// 打印单条助手消息
pub fn print_assistant_message(msg: &Message) {
    println!("{GREEN_BOLD}助手{RESET}");

    let reasoning = msg.reasoning_content.trim();
    if !reasoning.is_empty() {
        let summary: String = if reasoning.chars().count() > 80 {
            let truncated: String = reasoning.chars().take(77).collect();
            format!("{truncated}...")
        } else {
            reasoning.to_string()
        };
        println!("  {DIM}[思考] {summary}{RESET}");
    }

    let content = msg.content.trim();
    if content.is_empty() {
        println!("  {DIM}...{RESET}");
    } else {
        for line in content.lines() {
            println!("  {line}");
        }
    }
    println!();
}

/// 打印系统/工具消息
pub fn print_system_message(msg: &Message) {
    let content = msg.content.trim();
    if content.is_empty() {
        return;
    }
    for line in content.lines() {
        println!("{DIM}{line}{RESET}");
    }
    println!();
}

/// 打印会话历史中的所有消息
pub fn print_session_messages(messages: &[Message]) {
    for msg in messages {
        match msg.role {
            MessageRole::User => print_user_message(&msg.content),
            MessageRole::Assistant => print_assistant_message(msg),
            MessageRole::System => print_system_message(msg),
        }
    }
}

/// 打印错误
pub fn print_error(msg: &str) {
    eprintln!("{RED}{msg}{RESET}");
}

/// 打印信息
pub fn print_info(msg: &str) {
    println!("{msg}");
}

/// 打印警告
pub fn print_warn(msg: &str) {
    println!("{YELLOW}{msg}{RESET}");
}

/// 打印状态信息（灰色）
pub fn print_status(msg: &str) {
    println!("{DIM}{msg}{RESET}");
}

/// 打印流式解释文本增量（不换行，直接追加输出）
pub fn print_delta(delta: &str) {
    use std::io::Write;
    print!("{delta}");
    let _ = std::io::stdout().flush();
}

/// 确保光标在新行
pub fn flush_line() {
    println!();
}

