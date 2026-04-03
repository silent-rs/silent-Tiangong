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

    // 如果有 reasoning_content，折叠显示
    let reasoning = msg.reasoning_content.trim();
    if !reasoning.is_empty() {
        let summary = reasoning_summary(reasoning);
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

/// 打印工具执行摘要（单行简洁格式）
pub fn print_tool_brief(content: &str) {
    // 从工具执行系统消息中提取工具名和状态
    let name = content
        .lines()
        .find_map(|l| l.strip_prefix("tool_name: "))
        .unwrap_or("tool");
    let ok = if content.contains("ok=true") {
        format!("{GREEN_BOLD}OK{RESET}")
    } else {
        format!("{RED}FAIL{RESET}")
    };
    println!("{DIM}  ⏺ {name} {ok}{RESET}");
}

/// 打印 LLM 输出系统消息中的解释文本（提取首行之后的内容）
pub fn print_llm_explanation(content: &str) {
    // LLM 输出消息格式: "LLM 输出 [react-round-N]\n内容..."
    // 执行完成后替换为: "LLM 输出 [react-round-N]\ntokens: ...\ntool_calls: ...\ncontent:\n实际内容"
    if content.contains("\ntokens:") {
        // 已完成的 LLM 输出，提取 content: 后的实际内容
        let content_idx = content.find("\ncontent:\n");
        if let Some(idx) = content_idx {
            let actual = content[idx + "\ncontent:\n".len()..].trim();
            if !actual.is_empty() {
                for line in actual.lines() {
                    println!("{DIM}  {line}{RESET}");
                }
            }
        }
    } else {
        // 还在流式阶段，首行是 "LLM 输出 [...]"，后续是实际内容
        if let Some(idx) = content.find('\n') {
            let thinking = content[idx + 1..].trim();
            if !thinking.is_empty() {
                for line in thinking.lines() {
                    println!("{DIM}  {line}{RESET}");
                }
            }
        }
    }
}

/// 对 reasoning 内容生成摘要（只取前几个字）
fn reasoning_summary(reasoning: &str) -> String {
    let first_line = reasoning.lines().next().unwrap_or("");
    if first_line.len() > 60 {
        format!("{}...", &first_line[..57])
    } else if reasoning.lines().count() > 1 {
        format!("{first_line}...")
    } else {
        first_line.to_string()
    }
}
