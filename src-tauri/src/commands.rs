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
    build_session_snapshot(core_state, core_state.active_session_id())
}

fn build_session_snapshot(
    core_state: &tiangong_core::app_state::TiangongState,
    session_id: &str,
) -> RunSnapshot {
    let core_snapshot = core_state.run_snapshot();
    let input_draft = core_state.input_draft().to_string();

    // 获取指定会话的消息
    let messages: Vec<Message> = core_state
        .sessions()
        .iter()
        .find(|s| s.id == session_id)
        .map(|s| s.messages.iter().map(Message::from_core).collect())
        .unwrap_or_default();

    // 获取当前执行的计划（从 active_task_plans 获取第一个进行中的计划）
    let current_plan = core_state
        .active_task_plans()
        .first()
        .map(TaskPlan::from_session_task_plan);

    let pending_session_ids = core_state.pending_session_ids();

    let mut snapshot = RunSnapshot::from_core_with_session(
        core_snapshot,
        messages,
        input_draft,
        current_plan,
        pending_session_ids,
    );

    // 按 session 修正 status：如果该 session 没有 pending_turn，状态应为 idle
    if core_state.has_pending_turn_for(session_id) {
        // 该 session 正在运行，但全局 RunSnapshot 可能被其他 session 的事件覆盖
        // 如果 last_session_id 不匹配，给一个合理的默认状态
        if snapshot.last_session_id.as_deref() != Some(session_id) {
            snapshot.status = "executing".to_string();
            snapshot.summary = "正在执行中".to_string();
        }
    } else {
        // 该 session 没有在运行
        snapshot.status = "idle".to_string();
        // 保留 summary 供历史查看，但清除执行中相关的字段
        snapshot.current_plan = None;
    }

    snapshot
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
            .ok_or_else(|| anyhow::anyhow!("Failed to create session"))
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
    // 记录发送时的 session_id
    let session_id = state
        .with_state_read(|core_state| Ok(core_state.active_session_id().to_string()))?;

    // 设置输入草稿并发送
    state.with_state(|core_state| {
        core_state.update_draft(content);
        core_state.send_current_input()
    })?;

    // 启动后台任务监听事件并推送到前端
    let app_clone = app.clone();
    let poll_session_id = session_id.clone();
    thread::spawn(move || {
        loop {
            // 单次持锁：轮询事件 + 构建快照 + 检查完成状态
            let poll_result = app_clone
                .state::<TiangongApp>()
                .with_state(|core_state| {
                    // 1. 轮询本次 session 的事件
                    core_state.poll_pending_turn_for(&poll_session_id);

                    // 2. 构建快照
                    let snapshot = build_full_snapshot(core_state);

                    // 3. 检查是否完成
                    let done = !core_state.has_pending_turn_for(&poll_session_id);

                    // 4. 完成后重置为 idle
                    if done {
                        let status_str =
                            format!("{:?}", core_state.run_snapshot().status).to_lowercase();
                        if status_str == "completed" {
                            core_state.report_run_idle(format!(
                                "模型供应商：{}",
                                core_state.provider_label()
                            ));
                        } else if status_str == "failed" {
                            core_state.report_run_idle("执行失败");
                        }
                    }

                    Ok((snapshot, done))
                });

            match poll_result {
                Ok((snapshot, done)) => {
                    let _ = app_clone.emit("run_snapshot", &snapshot);

                    if done {
                        // 发送最终 idle 快照
                        if let Ok(final_snapshot) = app_clone
                            .state::<TiangongApp>()
                            .with_state_read(|s| Ok(build_full_snapshot(s)))
                        {
                            let _ = app_clone.emit("run_snapshot", &final_snapshot);
                        }
                        break;
                    }
                }
                Err(_) => break,
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

/// 获取后台任务列表
#[tauri::command]
pub fn get_background_tasks() -> Result<Vec<serde_json::Value>, String> {
    let reg = tiangong_core::tool::background_task::task_registry();
    let mut guard = reg.lock().map_err(|e| e.to_string())?;
    let tasks = guard.list();
    tasks
        .into_iter()
        .map(|t| serde_json::to_value(t).map_err(|e| e.to_string()))
        .collect()
}

/// 取消后台任务
#[tauri::command]
pub fn cancel_background_task(task_id: String) -> Result<(), String> {
    let reg = tiangong_core::tool::background_task::task_registry();
    let mut guard = reg.lock().map_err(|e| e.to_string())?;
    guard.cancel(&task_id);
    Ok(())
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

/// 获取活动会话的工作目录
#[tauri::command]
pub fn get_session_cwd(state: State<TiangongApp>) -> Result<String, String> {
    state.with_state_read(|core_state| Ok(core_state.active_session_cwd().to_string()))
}

/// 设置活动会话的工作目录
#[tauri::command]
pub fn set_session_cwd(cwd: String, state: State<TiangongApp>) -> Result<(), String> {
    // 验证路径存在且是目录
    let path = std::path::Path::new(&cwd);
    if !path.is_dir() {
        return Err(format!("路径不存在或不是目录：{cwd}"));
    }
    state.with_state(|core_state| core_state.update_active_session_cwd(cwd))
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

/// 获取 MCP 服务器健康状态
#[tauri::command]
pub fn get_mcp_health() -> Result<Vec<serde_json::Value>, String> {
    let statuses = tiangong_core::mcp::mcp_server_health_statuses();
    statuses
        .into_iter()
        .map(|s| serde_json::to_value(s).map_err(|e| e.to_string()))
        .collect()
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

/// 检查 Skill 安装需求（返回需要配置的环境变量列表）
#[tauri::command]
pub fn inspect_skill(path: String, state: State<TiangongApp>) -> Result<SkillInspection, String> {
    state.with_state_read(|core_state| {
        let inspection = core_state.inspect_skill_install_requirements(&path, true)?;
        Ok(SkillInspection {
            env_vars: inspection.env_vars,
            missing_env_vars: inspection.missing_env_vars,
            dependencies: inspection.dependencies,
        })
    })
}

/// 安装 Skill（支持传入环境变量配置）
#[tauri::command]
pub fn install_skill(
    path: String,
    env_values: Option<std::collections::HashMap<String, String>>,
    state: State<TiangongApp>,
) -> Result<String, String> {
    state.with_state(|core_state| {
        let env: Vec<(String, String)> = env_values
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .collect();
        core_state.install_local_skill_with_options_and_inputs(&path, true, true, &env)
    })
}

/// 移除 Skill
#[tauri::command]
pub fn remove_skill(id: String, state: State<TiangongApp>) -> Result<String, String> {
    state.with_state(|core_state| core_state.remove_skill(&id))
}

/// 获取 Skill 的环境变量（合并 skill.toml 声明的 requires.env + .env.local 已有值）
#[tauri::command]
pub fn get_skill_env(id: String, state: State<TiangongApp>) -> Result<std::collections::HashMap<String, String>, String> {
    state.with_state_read(|core_state| {
        let skill = core_state.installed_skills()
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;

        let skill_dir = std::path::Path::new(&skill.source.value);
        let mut env = std::collections::HashMap::new();

        // 1. 从 skill.toml 的 requires.env 读取声明的 key（值为空）
        let toml_path = skill_dir.join("skill.toml");
        if let Ok(raw) = std::fs::read_to_string(&toml_path) {
            #[derive(serde::Deserialize, Default)]
            struct T { #[serde(default)] requires: R }
            #[derive(serde::Deserialize, Default)]
            struct R { #[serde(default)] env: Vec<String> }
            if let Ok(parsed) = toml::from_str::<T>(&raw) {
                for key in parsed.requires.env {
                    env.insert(key, String::new());
                }
            }
        }

        // 2. 从 .env.local 读取已有值（覆盖空值）
        let env_path = skill_dir.join(".env.local");
        if let Ok(content) = std::fs::read_to_string(&env_path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                if let Some((k, v)) = line.split_once('=') {
                    env.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }

        Ok(env)
    })
}

/// 设置 Skill 的环境变量
#[tauri::command]
pub fn set_skill_env(
    id: String,
    env: std::collections::HashMap<String, String>,
    state: State<TiangongApp>,
) -> Result<(), String> {
    state.with_state_read(|core_state| {
        let skill = core_state.installed_skills()
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;
        let env_path = std::path::Path::new(&skill.source.value).join(".env.local");
        let lines: Vec<String> = env.iter()
            .filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty())
            .map(|(k, v)| format!("{}={}", k.trim(), v.trim()))
            .collect();
        if lines.is_empty() {
            let _ = std::fs::remove_file(&env_path);
        } else {
            std::fs::write(&env_path, format!("{}\n", lines.join("\n")))
                .map_err(|e| anyhow::anyhow!("写入 .env.local 失败：{e}"))?;
        }
        Ok(())
    })
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

/// 根据 provider 配置获取该 provider 的可用模型列表
#[tauri::command]
pub fn fetch_provider_models(
    base_url: String,
    api_key: String,
    timeout_ms: Option<u64>,
) -> Result<Vec<String>, String> {
    use tiangong_core::model::{ModelProviderConfig, SingleProviderClient};
    use tiangong_core::models_config::ModelsConfig;

    let resolved_key = ModelsConfig::resolve_api_key(&api_key);
    let config = ModelProviderConfig {
        api_auth_token: resolved_key,
        api_base_url: base_url,
        api_timeout_ms: timeout_ms.unwrap_or(60_000).to_string(),
        api_model: String::new(),
        api_lite_model: String::new(),
    };
    SingleProviderClient::list_models(&config).map_err(|e| e.to_string())
}
