use anyhow::{Result, anyhow};
use tiangong_core::app_state::TiangongState;

use crate::completion;
use crate::modal;
use crate::output;

/// 处理 / 命令，返回 true 表示应退出
/// draft_new_session: 切换会话时重置为 false
pub fn handle_command(
    state: &mut TiangongState,
    command: &str,
    draft_new_session: &mut bool,
) -> Result<bool> {
    let command = command.trim();

    match command {
        "/exit" | "/quit" | "/q" => return Ok(true),
        "/help" | "/h" | "/?" => print_help(),
        "/new" => {
            *draft_new_session = true;
            output::print_status("已打开新对话（发送首条消息后才会记录）");
        }
        _ if command == "/history"
            || command == "/sessions"
            || command.starts_with("/history ")
            || command.starts_with("/sessions ") =>
        {
            let arg = command
                .trim_start_matches("/history")
                .trim_start_matches("/sessions")
                .trim();
            handle_sessions(state, arg, draft_new_session)?;
        }
        _ if command == "/model" || command.starts_with("/model ") => {
            handle_model(state, command)?;
        }
        "/mcp" => {
            modal::mcp::open(state)?;
        }
        "/skill" => {
            modal::skill::open(state)?;
        }
        "/cancel" => {
            if state.cancel_pending_turn()? {
                output::print_status("已取消当前任务");
            } else {
                output::print_warn("当前没有可取消的任务");
            }
        }
        _ if command == "/config" || command.starts_with("/config ") => {
            handle_config(state, command)?;
        }
        _ => {
            output::print_warn(&format!("未知命令：{command}，输入 /help 查看可用命令"));
        }
    }

    Ok(false)
}

fn print_help() {
    output::print_info(&completion::help_text());
}

fn handle_sessions(
    state: &mut TiangongState,
    arg: &str,
    draft_new_session: &mut bool,
) -> Result<()> {
    if arg.is_empty() {
        if let Some(_id) = modal::sessions::open(state)? {
            *draft_new_session = false;
            let title = state
                .active_session()
                .map(|s| s.title.as_str())
                .unwrap_or("未知");
            output::print_status(&format!("已切换会话：{title}"));
            if let Some(session) = state.active_session()
                && !session.messages.is_empty()
            {
                output::print_session_messages(&session.messages);
            }
        }
        return Ok(());
    }

    // 按序号或 ID 前缀切换
    let sessions = state.sessions();
    let target_id = if let Ok(idx) = arg.parse::<usize>() {
        sessions
            .get(idx.saturating_sub(1))
            .map(|s| s.id.clone())
            .ok_or_else(|| anyhow!("序号超出范围：{idx}"))?
    } else {
        sessions
            .iter()
            .find(|s| s.id.starts_with(arg))
            .map(|s| s.id.clone())
            .ok_or_else(|| anyhow!("未找到匹配的会话：{arg}"))?
    };

    let title = sessions
        .iter()
        .find(|s| s.id == target_id)
        .map(|s| s.title.clone())
        .unwrap_or_default();

    state.switch_session(&target_id);
    *draft_new_session = false;
    output::print_status(&format!("已切换会话：{title}"));

    if let Some(session) = state.active_session()
        && !session.messages.is_empty()
    {
        output::print_session_messages(&session.messages);
    }

    Ok(())
}

fn handle_model(state: &mut TiangongState, command: &str) -> Result<()> {
    let arg = command.trim_start_matches("/model").trim();

    if arg.is_empty() {
        let current = state.current_model();
        let models = state.model_list();
        output::print_info(&format!("当前模型：{current}"));
        if !models.is_empty() {
            output::print_info("可用模型：");
            for m in models {
                let marker = if m == current { " *" } else { "" };
                output::print_info(&format!("  {m}{marker}"));
            }
        }
        output::print_status("使用 /model <名称> 切换模型");
        return Ok(());
    }

    state.select_model(arg)?;
    output::print_status(&format!("模型已切换为：{arg}"));
    Ok(())
}

fn handle_config(state: &mut TiangongState, command: &str) -> Result<()> {
    let args = command.trim_start_matches("/config").trim();

    if args.is_empty() || args == "show" {
        output::print_info(&state.agent_config_summary());
        return Ok(());
    }

    if args == "validate" {
        state.validate_agent_config()?;
        output::print_status("配置校验通过");
        return Ok(());
    }

    if let Some(raw_set) = args.strip_prefix("set ") {
        let raw_set = raw_set.trim();
        let mut parts = raw_set.splitn(2, char::is_whitespace);
        let key = parts
            .next()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("缺少配置键"))?;
        let value = parts
            .next()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("缺少配置值"))?;
        let message = state.update_agent_config_entry(key, value)?;
        output::print_status(&message);
        return Ok(());
    }

    Err(anyhow!(
        "不支持的 /config 命令。可用：/config show、/config validate、/config set <key> <value>"
    ))
}
