use std::io::{self, Write};

use anyhow::Result;

use crate::core::model::{ModelProviderConfig, SingleProviderClient};
use crate::core::runtime::RuntimeEngine;
use crate::core::session::{MessageRole, Session};

const CLI_CONTEXT_LIMIT: usize = 16;

pub fn run_chat() -> Result<()> {
    let cfg = ModelProviderConfig::from_env();
    let runtime = RuntimeEngine::new(SingleProviderClient::new(cfg), CLI_CONTEXT_LIMIT);
    let mut session = Session::new("CLI 会话");

    println!("天工 CLI 对话模式已启动");
    println!("输入 /help 查看命令，输入 /exit 退出。");

    let stdin = io::stdin();
    let mut line = String::new();

    loop {
        print!("你> ");
        io::stdout().flush()?;
        line.clear();

        let read_bytes = stdin.read_line(&mut line)?;
        if read_bytes == 0 {
            println!();
            break;
        }

        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }

        match input.as_str() {
            "/exit" | "/quit" => break,
            "/help" => {
                println!("/help  查看帮助");
                println!("/new   新建会话（清空上下文）");
                println!("/exit  退出 CLI");
                continue;
            }
            "/new" => {
                session = Session::new("CLI 会话");
                println!("已创建新会话。");
                continue;
            }
            _ => {}
        }

        session.append_message(MessageRole::User, input.clone());
        print!("天工> ");
        io::stdout().flush()?;

        let mut printed_any = false;
        match runtime.execute_turn_with_streaming(&session, &input, |delta| {
            if delta.is_empty() {
                return;
            }
            printed_any = true;
            print!("{delta}");
            let _ = io::stdout().flush();
        }) {
            Ok(exec) => {
                if !printed_any {
                    print!("{}", exec.assistant_message);
                }
                println!();
                session.append_message(MessageRole::Assistant, exec.assistant_message);
            }
            Err(err) => {
                println!();
                let err_msg = RuntimeEngine::fallback_error_message(&err);
                println!("[错误] {err_msg}");
                session.append_message(MessageRole::System, err_msg);
            }
        }
    }

    println!("已退出天工 CLI。");
    Ok(())
}
