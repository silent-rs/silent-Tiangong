use crate::app::TiangongApp;
use crate::types::*;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, Window};

// ============================================================================
// 辅助函数：构建完整的 RunSnapshot
// ============================================================================

fn build_full_snapshot(core_state: &tiangong_core::app_state::TiangongState) -> RunSnapshot {
    let core_snapshot = core_state.run_snapshot();
    let input_draft = core_state.input_draft().to_string();

    // 获取当前活动的会话消息
    let messages: Vec<Message> = core_state
        .active_session()
        .map(|s| s.messages.iter().map(Message::from_core).collect())
        .unwrap_or_default();

    // 获取当前执行的计划（从 active_task_plans 获取第一个进行中的计划）
    let current_plan = core_state
        .active_task_plans()
        .first()
        .map(TaskPlan::from_session_task_plan);

    RunSnapshot::from_core_with_session(core_snapshot, messages, input_draft, current_plan)
}

// ============================================================================
// 会话管理
// ============================================================================

/// 获取所有会话列表
#[tauri::command]
pub fn get_sessions(state: State<TiangongApp>) -> Result<Vec<Session>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .sessions()
            .iter()
            .map(Session::from_core)
            .collect())
    })
}

/// 创建新会话
#[tauri::command]
pub fn create_session(state: State<TiangongApp>) -> Result<Session, String> {
    state.with_state(|core_state| {
        core_state.create_session();
        // 返回新创建的活动会话
        core_state
            .active_session()
            .map(Session::from_core)
            .ok_or_else(|| anyhow::anyhow!("Failed to create session").into())
    })
}

/// 切换到指定会话
#[tauri::command]
pub fn switch_session(session_id: String, state: State<TiangongApp>) -> Result<(), String> {
    state.with_state(|core_state| {
        core_state.switch_session(&session_id);
        Ok(())
    })
}

/// 删除当前会话
#[tauri::command]
pub fn delete_session(state: State<TiangongApp>) -> Result<(), String> {
    state.with_state(|core_state| core_state.delete_active_session())
}

/// 更新会话标题
#[tauri::command]
pub fn update_session_title(title: String, state: State<TiangongApp>) -> Result<(), String> {
    state.with_state(|core_state| {
        core_state.update_session_title_draft(title);
        core_state.save_active_session_title()
    })
}

// ============================================================================
// 消息和执行
// ============================================================================

/// 发送消息并执行
#[tauri::command]
pub fn send_message(
    content: String,
    app: AppHandle,
    _window: Window,
    state: State<TiangongApp>,
) -> Result<(), String> {
    // 设置输入草稿并发送
    state.with_state(|core_state| {
        core_state.update_draft(content);
        core_state.send_current_input()
    })?;

    // 启动后台任务监听事件并推送到前端
    let app_clone = app.clone();
    thread::spawn(move || {
        loop {
            // 获取当前完整状态快照
            if let Ok(snapshot) = app_clone
                .state::<TiangongApp>()
                .with_state_read(|s| Ok(build_full_snapshot(s)))
            {
                let is_idle = snapshot.status == "idle"
                    || snapshot.status == "completed"
                    || snapshot.status == "failed";

                // 每次轮询都发送快照，确保消息内容变化也能同步
                let _ = app_clone.emit("run_snapshot", &snapshot);

                // 如果状态已结束，停止轮询
                if is_idle {
                    break;
                }
            }

            thread::sleep(Duration::from_millis(200));
        }
    });

    Ok(())
}

/// 取消当前执行
#[tauri::command]
pub fn cancel_turn(state: State<TiangongApp>) -> Result<bool, String> {
    state.with_state(|core_state| core_state.cancel_pending_turn())
}

/// 获取运行状态快照
#[tauri::command]
pub fn get_run_snapshot(state: State<TiangongApp>) -> Result<RunSnapshot, String> {
    state.with_state_read(|core_state| Ok(build_full_snapshot(core_state)))
}

/// 获取输入草稿
#[tauri::command]
pub fn get_input_draft(state: State<TiangongApp>) -> Result<String, String> {
    state.with_state_read(|core_state| Ok(core_state.input_draft().to_string()))
}

/// 设置输入草稿
#[tauri::command]
pub fn set_input_draft(content: String, state: State<TiangongApp>) -> Result<(), String> {
    state.with_state(|core_state| {
        core_state.update_draft(content);
        Ok(())
    })
}

// ============================================================================
// MCP 管理
// ============================================================================

/// 获取 MCP 服务器列表
#[tauri::command]
pub fn get_mcp_servers(state: State<TiangongApp>) -> Result<Vec<McpServer>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .mcp_servers()
            .iter()
            .map(McpServer::from_core)
            .collect())
    })
}

/// 注册 MCP 服务器
#[tauri::command]
pub fn register_mcp_server(
    name: String,
    command: String,
    args: Vec<String>,
    env: Option<std::collections::HashMap<String, String>>,
    state: State<TiangongApp>,
) -> Result<String, String> {
    use tiangong_core::app_state::RegisterMcpServerOptions;
    use tiangong_core::app_state::RegisterMcpServerRequest;

    state.with_state(|core_state| {
        // 转换 env HashMap 为 Vec<(String, String)>
        let env_vec = env.unwrap_or_default().into_iter().collect();

        let request = RegisterMcpServerRequest {
            name: name.clone(),
            command,
            args,
            tags: vec![],
            enabled: true,
            options: RegisterMcpServerOptions {
                env: env_vec,
                ..Default::default()
            },
        };
        core_state.register_mcp_server(request)
    })
}

/// 移除 MCP 服务器
#[tauri::command]
pub fn remove_mcp_server(name: String, state: State<TiangongApp>) -> Result<String, String> {
    state.with_state(|core_state| core_state.remove_mcp_server(&name))
}

/// 设置 MCP 服务器启用状态
#[tauri::command]
pub fn set_mcp_server_enabled(
    name: String,
    enabled: bool,
    state: State<TiangongApp>,
) -> Result<String, String> {
    state.with_state(|core_state| core_state.set_mcp_server_enabled(&name, enabled))
}

// ============================================================================
// Skill 管理
// ============================================================================

/// 获取已安装的 Skill 列表
#[tauri::command]
pub fn get_skills(state: State<TiangongApp>) -> Result<Vec<Skill>, String> {
    state.with_state_read(|core_state| {
        Ok(core_state
            .installed_skills()
            .iter()
            .map(Skill::from_core)
            .collect())
    })
}

/// 安装 Skill
#[tauri::command]
pub fn install_skill(path: String, state: State<TiangongApp>) -> Result<String, String> {
    state.with_state(|core_state| core_state.install_local_skill(&path, true))
}

/// 移除 Skill
#[tauri::command]
pub fn remove_skill(id: String, state: State<TiangongApp>) -> Result<String, String> {
    state.with_state(|core_state| core_state.remove_skill(&id))
}

/// 设置 Skill 启用状态
#[tauri::command]
pub fn set_skill_enabled(
    id: String,
    enabled: bool,
    state: State<TiangongApp>,
) -> Result<String, String> {
    state.with_state(|core_state| core_state.set_skill_enabled(&id, enabled))
}

// ============================================================================
// Server 管理
// ============================================================================

/// 获取 Server 配置
#[tauri::command]
pub fn get_server_config() -> Result<ServerConfigView, String> {
    let config = tiangong_server::config::load_server_config();
    let running = is_server_running();
    let auth_token_masked = config.masked_auth_token();
    Ok(ServerConfigView {
        host: config.host,
        port: config.port,
        auth_token_masked,
        running,
    })
}

/// 设置 Server 配置
#[tauri::command]
pub fn set_server_config(
    host: String,
    port: u16,
    auth_token: Option<String>,
) -> Result<String, String> {
    let config = tiangong_server::config::ServerConfig {
        host,
        port,
        auth_token,
    };
    tiangong_server::config::save_server_config(&config).map_err(|e| e.to_string())?;
    Ok("Server 配置已保存".to_string())
}

/// 检查 Server 是否在运行（通过 PID 文件判断）
fn is_server_running() -> bool {
    let pid_path = user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("server.pid");
    if !pid_path.exists() {
        return false;
    }
    match std::fs::read_to_string(&pid_path) {
        Ok(pid_str) => {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                // 检查进程是否存在
                #[cfg(unix)]
                {
                    use std::process::Command;
                    Command::new("kill")
                        .arg("-0")
                        .arg(pid.to_string())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    let _ = pid;
                    false
                }
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// 获取用户 home 目录
fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) =
        std::env::var_os("USERPROFILE").filter(|v| v != std::ffi::OsStr::new(""))
    {
        return Some(PathBuf::from(profile));
    }
    None
}

// ============================================================================
// Connector 管理
// ============================================================================

/// 获取 Connector 列表
#[tauri::command]
pub fn get_connectors() -> Result<Vec<ConnectorInfo>, String> {
    let configs = tiangong_server::config::load_connectors_config();
    Ok(configs.iter().map(ConnectorInfo::from_config).collect())
}

/// 设置 Connector 启用状态
#[tauri::command]
pub fn set_connector_enabled(name: String, enabled: bool) -> Result<String, String> {
    tiangong_server::config::set_connector_enabled(&name, enabled).map_err(|e| e.to_string())?;
    Ok(format!(
        "Connector \"{}\" 已{}",
        name,
        if enabled { "启用" } else { "禁用" }
    ))
}

// ============================================================================
// 模型配置（Provider + Model + Routing 三层架构）
// ============================================================================

/// 获取模型配置
#[tauri::command]
pub fn get_models_config(state: State<TiangongApp>) -> Result<ModelsConfigView, String> {
    state.with_state_read(|core_state| {
        Ok(ModelsConfigView::from_core(core_state.models_config()))
    })
}

/// 设置模型配置
#[tauri::command]
pub fn set_models_config(
    config: ModelsConfigView,
    state: State<TiangongApp>,
) -> Result<(), String> {
    state.with_state(|core_state| {
        let core_config = config.to_core();
        core_state.save_models_config(core_config)
    })
}

/// 获取所有可用的模型能力列表
#[tauri::command]
pub fn get_model_capabilities() -> Result<Vec<ModelCapabilityInfo>, String> {
    use tiangong_core::models_config::ModelCapability;

    let caps = ModelCapability::all()
        .iter()
        .map(|c| {
            let key = serde_json::to_value(c).unwrap_or_default();
            ModelCapabilityInfo {
                key: key.as_str().unwrap_or_default().to_string(),
                display_name: c.display_name().to_string(),
            }
        })
        .collect();
    Ok(caps)
}

/// 获取模型列表
#[tauri::command]
pub fn get_model_list(state: State<TiangongApp>) -> Result<Vec<String>, String> {
    state.with_state_read(|core_state| Ok(core_state.model_list().to_vec()))
}
