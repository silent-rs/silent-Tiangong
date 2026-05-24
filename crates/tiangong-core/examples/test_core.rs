use std::sync::mpsc;
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
use tiangong_types::{SessionStreamEvent, StreamEvent};

fn main() {
    // 示例：使用默认配置（第三方开发者可直接构造 CoreConfig）
    let config = CoreConfigProvider::new(CoreConfig::default());
    let (tx, rx) = mpsc::channel::<SessionStreamEvent>();
    let core = TiangongCore::new(config, tx);

    println!("=== 发送: 你好 ===");
    core.send_message("你好".into());

    let mut got_done = false;
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(se) => match &se.event {
                StreamEvent::UserMessage { content, .. } => println!("[用户] {content}"),
                StreamEvent::Delta { content: text, .. } => print!("{text}"),
                StreamEvent::Reasoning { .. } => print!("[R]"),
                StreamEvent::ToolCalls { names, .. } => println!("\n[调用] {}", names.join(",")),
                StreamEvent::ToolStart { name, .. } => println!("[开始] {name}"),
                StreamEvent::ToolResult { name, ok, .. } => println!("[结果] {name} ok={ok}"),
                StreamEvent::Done { .. } => {
                    println!("\n=== Done ===");
                    got_done = true;
                    break;
                }
                StreamEvent::Error { message: err } => {
                    println!("\n=== Error: {err} ===");
                    break;
                }
                _ => {}
            },
            Err(_) => {
                println!("\n=== 超时 ===");
                break;
            }
        }
    }

    println!("got_done={got_done}");

    if got_done {
        println!("\n=== 发送: 1+1=? ===");
        core.send_message("1+1=?".into());

        loop {
            match rx.recv_timeout(std::time::Duration::from_secs(30)) {
                Ok(se) => match &se.event {
                    StreamEvent::Delta { content: text, .. } => print!("{text}"),
                    StreamEvent::Reasoning { .. } => print!("[R]"),
                    StreamEvent::Done { .. } => {
                        println!("\n=== 第二轮 Done ===");
                        break;
                    }
                    StreamEvent::Error { message: err } => {
                        println!("\n=== 第二轮 Error: {err} ===");
                        break;
                    }
                    _ => {}
                },
                Err(_) => {
                    println!("\n=== 第二轮超时 ===");
                    break;
                }
            }
        }
    }

    println!("\n=== shutdown ===");
    let session = core.into_session();
    println!("消息数: {}", session.messages.len());
    for (i, m) in session.messages.iter().enumerate() {
        let text = m.text_content();
        let preview: String = text.chars().take(80).collect();
        println!(
            "[{i}] {:?} clen={} | {}",
            m.role,
            text.len(),
            preview.replace('\n', "\\n")
        );
    }
}
