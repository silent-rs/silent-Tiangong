use anyhow::{Result, anyhow};
use tiangong_app_state::app_state::TiangongState;
use tiangong_core::core_config::CoreConfigProvider;

use crate::completion;
use crate::modal;
use crate::output;

/// 处理 / 命令，返回 true 表示应退出。
pub fn handle_command(
    state: &mut TiangongState,
    config: &CoreConfigProvider,
    command: &str,
    storage_root: &std::path::Path,
) -> Result<bool> {
    let command = command.trim();
    let mut sync_core_config = false;

    match command {
        "/exit" | "/quit" | "/q" => return Ok(true),
        "/help" | "/h" | "/?" => print_help(),
        "/new" => {
            state.active_session_id = scru128::new().to_string();
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
            handle_sessions(state, arg)?;
        }
        _ if command == "/model" || command.starts_with("/model ") => {
            handle_model(state, command)?;
            sync_core_config = true;
        }
        "/mcp" => {
            modal::mcp::open()?;
            sync_core_config = true;
        }
        "/skill" => {
            modal::skill::open(state, storage_root)?;
            sync_core_config = true;
        }
        "/cancel" => {
            if state.core_manager.cancel_core(&state.active_session_id) {
                output::print_status("已取消当前任务");
            } else {
                output::print_warn("当前没有可取消的任务");
            }
        }
        _ if command == "/config" || command.starts_with("/config ") => {
            handle_config(state, command)?;
            sync_core_config = command
                .trim_start_matches("/config")
                .trim()
                .starts_with("set ");
        }
        _ => {
            output::print_warn(&format!("未知命令：{command}，输入 /help 查看可用命令"));
        }
    }

    if sync_core_config {
        let next = state.config.to_core_config();
        config.replace(next);
    }

    Ok(false)
}

fn print_help() {
    output::print_info(&completion::help_text());
}

fn handle_sessions(state: &mut TiangongState, arg: &str) -> Result<()> {
    let core_manager = state.core_manager.clone();
    if arg.is_empty() {
        if let Some(_id) = modal::sessions::open(state)? {
            let title = core_manager
                .load_session(&state.active_session_id)
                .ok()
                .map(|s| s.title)
                .unwrap_or_else(|| "未知".to_string());
            output::print_status(&format!("已切换会话：{title}"));
            if let Some(session) = core_manager.load_session(&state.active_session_id).ok()
                && !session.messages.is_empty()
            {
                output::print_session_messages(&session.messages);
            }
        }
        return Ok(());
    }

    // 按序号或 ID 前缀切换
    let sessions = core_manager.list_session_metadata();
    let target_id = if let Ok(idx) = arg.parse::<usize>() {
        sessions
            .get(idx.saturating_sub(1))
            .map(|m| m.id.clone())
            .ok_or_else(|| anyhow!("序号超出范围：{idx}"))?
    } else {
        sessions
            .iter()
            .find(|m| m.id.starts_with(arg))
            .map(|m| m.id.clone())
            .ok_or_else(|| anyhow!("未找到匹配的会话：{arg}"))?
    };

    let title = sessions
        .iter()
        .find(|m| m.id == target_id)
        .map(|m| m.title.clone())
        .unwrap_or_default();

    state.active_session_id = target_id.clone();
    output::print_status(&format!("已切换会话：{title}"));

    if let Some(session) = core_manager.load_session(&target_id).ok()
        && !session.messages.is_empty()
    {
        output::print_session_messages(&session.messages);
    }

    Ok(())
}

fn handle_model(state: &mut TiangongState, command: &str) -> Result<()> {
    let arg = command.trim_start_matches("/model").trim();

    if arg.is_empty() {
        let current = current_chat_model(state);
        let models = &state.model_list;
        output::print_info(&format!("当前模型：{current}"));
        if !models.is_empty() {
            output::print_info("可用模型：");
            for m in models {
                let marker = if *m == current { " *" } else { "" };
                output::print_info(&format!("  {m}{marker}"));
            }
        }
        output::print_status("使用 /model <名称> 切换模型");
        return Ok(());
    }

    select_model(state, arg)?;
    output::print_status(&format!("模型已切换为：{arg}"));
    Ok(())
}

/// 解析当前应用配置中的 Chat 路由模型名。
fn current_chat_model(state: &TiangongState) -> String {
    state
        .config
        .models
        .resolve_slot(tiangong_llm::models_config::RoutingSlot::Chat)
        .map(|r| r.model)
        .unwrap_or_default()
}

/// 切换 Chat 路由模型：配置写盘成功后刷新运行期状态。
fn select_model(state: &mut TiangongState, model: &str) -> Result<()> {
    let api_model = model.trim();
    if api_model.is_empty() {
        return Err(anyhow!("API_MODEL 不能为空"));
    }
    let mut models = state.config.models.clone();
    models.update_chat_model(api_model.to_string());
    let config = tiangong_config::registry::update_models(&state.config, models)?;
    state.config = config;
    let resolved = current_chat_model(state);
    if !state.model_list.contains(&resolved) {
        state.model_list.insert(0, resolved);
    }
    Ok(())
}

fn handle_config(state: &mut TiangongState, command: &str) -> Result<()> {
    let args = command.trim_start_matches("/config").trim();

    if args.is_empty() || args == "show" {
        // 经 sidecar 通道拉取 MCP 配置快照。
        let mcp: tiangong_plugin_mcp_protocol::config::McpConfig =
            serde_json::from_value(tiangong_plugin_runtime::registry::invoke_sidecar(
                &tiangong_config::io::storage_root(),
                "mcp",
                "mcp.config.snapshot",
                serde_json::json!({}),
            )?)?;
        output::print_info(&format!(
            "mcp.enabled={}, mcp.timeout_ms={}, mcp.servers={}, trust_mode={:?}",
            mcp.enabled,
            mcp.timeout_ms,
            mcp.servers.len(),
            state.agent_config.trust_mode,
        ));
        return Ok(());
    }

    if args == "validate" {
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
        use tiangong_plugin_mcp_protocol::config::UpdateConfigEntryRequest;
        let response: tiangong_plugin_mcp_protocol::MessageResponse =
            serde_json::from_value(tiangong_plugin_runtime::registry::invoke_sidecar(
                &tiangong_config::io::storage_root(),
                "mcp",
                "mcp.config.update_entry",
                serde_json::to_value(UpdateConfigEntryRequest {
                    key: key.to_string(),
                    value: value.to_string(),
                })?,
            )?)?;
        output::print_status(&response.message);
        return Ok(());
    }

    Err(anyhow!(
        "不支持的 /config 命令。可用：/config show、/config validate、/config set <key> <value>"
    ))
}
