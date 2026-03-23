use crate::app::TiangongApp;
use crate::types::*;
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
// 模型提供者
// ============================================================================

/// 获取模型配置
#[tauri::command]
pub fn get_model_config(state: State<TiangongApp>) -> Result<ModelConfig, String> {
    state.with_state_read(|core_state| {
        let config = core_state.model_config();
        Ok(ModelConfig {
            api_auth_token: config.api_auth_token.clone(),
            api_base_url: config.api_base_url.clone(),
            api_timeout_ms: config.api_timeout_ms.clone(),
            api_model: config.api_model.clone(),
        })
    })
}

/// 设置模型配置
#[tauri::command]
pub fn set_model_config(
    api_auth_token: Option<String>,
    api_base_url: Option<String>,
    api_timeout_ms: Option<String>,
    api_model: Option<String>,
    state: State<TiangongApp>,
) -> Result<(), String> {
    state.with_state(|core_state| {
        // 更新各个配置项
        if let Some(token) = api_auth_token {
            core_state.update_settings_api_auth_token_draft(token);
        }
        if let Some(base_url) = api_base_url {
            core_state.update_settings_api_base_url_draft(base_url);
        }
        if let Some(timeout) = api_timeout_ms {
            core_state.update_settings_api_timeout_ms_draft(timeout);
        }
        if let Some(model) = api_model {
            core_state.update_settings_api_model_draft(model);
        }

        // 保存配置
        core_state.save_provider_settings()
    })
}

/// 获取模型列表
#[tauri::command]
pub fn get_model_list(state: State<TiangongApp>) -> Result<Vec<String>, String> {
    state.with_state_read(|core_state| Ok(core_state.model_list().to_vec()))
}

/// 获取提供者列表（简化版，返回当前配置的提供者）
#[tauri::command]
pub fn get_providers(state: State<TiangongApp>) -> Result<Vec<Provider>, String> {
    state.with_state_read(|core_state| {
        let config = core_state.model_config();
        let models = core_state.model_list().to_vec();

        let provider = Provider {
            id: "default".to_string(),
            name: core_state.provider_label(),
            base_url: if config.api_base_url.is_empty() {
                None
            } else {
                Some(config.api_base_url.clone())
            },
            models: models
                .into_iter()
                .map(|name| Model {
                    id: name.clone(),
                    name,
                    provider_id: "default".to_string(),
                })
                .collect(),
        };

        Ok(vec![provider])
    })
}
