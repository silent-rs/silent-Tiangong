use std::collections::HashMap;
use std::sync::mpsc::Sender;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent_config::{AgentConfig, McpConfig};
use crate::agents::execution_mcp_agent::{
    McpFunctionTarget, execute_mcp_tool_call, resolve_mcp_tool_call_from_run_command,
};
use crate::agents::execution_tool_agent::build_tool_call_from_function;
use crate::app_state::{ManagementCommand, TurnEvent};
use crate::model::FunctionToolSpec;
use crate::model::{ModelClient, ModelFunctionCall, SingleProviderClient, TokenUsage};
use crate::models_config::{ModelCapability, ModelsConfig};
use crate::planner::TaskPlan;
use crate::tool::{LocalToolExecutor, ToolExecutionRecord, ToolExecutor, ToolResult};

pub use tiangong_types::RunStatus;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub status: RunStatus,
    pub summary: String,
    pub last_session_id: Option<String>,
    pub last_task_id: Option<String>,
    pub last_duration_ms: Option<u64>,
    pub last_result: Option<String>,
    pub last_plan: Option<String>,
    pub last_tool_result: Option<String>,
    pub last_error: Option<String>,
    pub last_usage: Option<TokenUsage>,
    pub updated_at: String,
    /// 等待审批的请求 ID（WaitingApproval 状态时有值）
    pub approval_request_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LlmOutputRecord {
    pub stage: String,
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<String>,
    pub usage: TokenUsage,
}

pub use crate::agents::response_agent::VerifyExecutionRecord;

#[derive(Debug, Clone)]
pub struct TurnExecution {
    pub assistant_message: String,
    pub assistant_reasoning_content: String,
    pub system_prompt: String,
    pub plan: TaskPlan,
    pub tool_result_summary: Option<String>,
    pub tool_execution: Option<ToolExecutionRecord>,
    pub verify_records: Vec<VerifyExecutionRecord>,
    pub output_mode: String,
    pub output_chunk_count: usize,
    pub usage: TokenUsage,
    /// 开发阶段：所有 LLM 调用的完整记录
    pub llm_calls: Vec<crate::session::LlmCallRecord>,
}

#[derive(Debug, Clone)]
pub struct RuntimeEngine {
    client: SingleProviderClient,
    tool_executor: LocalToolExecutor,
    pub context_limit: usize,
    agent_config: AgentConfig,
    models_config: ModelsConfig,
    permission_gate: crate::permission::PermissionGate,
}

impl RuntimeEngine {
    pub fn new(
        client: SingleProviderClient,
        context_limit: usize,
        agent_config: AgentConfig,
    ) -> Self {
        let permission_gate =
            crate::permission::PermissionGate::new(crate::permission::PermissionPolicy {
                trust_mode: agent_config.trust_mode,
                ..Default::default()
            });
        Self {
            client,
            tool_executor: LocalToolExecutor::from_agent_config(&agent_config),
            context_limit,
            agent_config,
            models_config: ModelsConfig::default(),
            permission_gate,
        }
    }

    /// 使用共享的信任模式引用创建（确保跨 clone 实例共享同一权限状态）
    pub fn with_shared_trust_mode(
        client: SingleProviderClient,
        context_limit: usize,
        agent_config: AgentConfig,
        shared_trust_mode: std::sync::Arc<std::sync::RwLock<crate::permission::TrustMode>>,
    ) -> Self {
        let permission_gate = crate::permission::PermissionGate::with_shared_trust_mode(
            crate::permission::PermissionPolicy {
                trust_mode: agent_config.trust_mode,
                ..Default::default()
            },
            shared_trust_mode.clone(),
        );
        Self {
            client,
            tool_executor: LocalToolExecutor::from_agent_config(&agent_config)
                .with_shared_trust_mode(shared_trust_mode),
            context_limit,
            agent_config,
            models_config: ModelsConfig::default(),
            permission_gate,
        }
    }

    pub fn with_models_config(mut self, config: ModelsConfig) -> Self {
        self.models_config = config;
        self
    }

    /// 获取模型客户端引用（供 TurnRunner 使用）
    pub fn client(&self) -> &SingleProviderClient {
        &self.client
    }
    /// 获取 AgentConfig 引用
    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent_config
    }
    /// 获取 ModelsConfig 引用
    pub fn models_config(&self) -> &ModelsConfig {
        &self.models_config
    }
    /// 获取权限网关引用
    pub fn permission_gate(&self) -> &crate::permission::PermissionGate {
        &self.permission_gate
    }

    pub fn provider_label(&self) -> String {
        format!(
            "{} @ {} · {}ms",
            self.client.api_model(),
            self.client.api_base_url(),
            self.client.api_timeout_ms()
        )
    }

    /// 执行单个工具调用（本地工具、MCP 工具、多媒体生成或后台任务）
    pub(crate) fn execute_tool_call(
        &self,
        call: &ModelFunctionCall,
        mcp_targets: &HashMap<String, McpFunctionTarget>,
        mcp_config: &McpConfig,
    ) -> ToolResult {
        // 权限检查
        use crate::permission::PermissionDecision;
        match self.permission_gate.check(&call.name) {
            PermissionDecision::Approved => {}
            PermissionDecision::Denied { reason } => {
                return ToolResult {
                    ok: false,
                    summary: format!("权限拒绝：{reason}"),
                    stdout: String::new(),
                    stderr: reason,
                    exit_code: 1,
                    execution: None,
                };
            }
            PermissionDecision::NeedsApproval { request_id } => {
                // Phase 3 中将改为暂停等待审批，当前在非 FullTrust 模式下记录日志并放行
                tracing::info!(
                    "权限审批请求 {request_id}：工具 {} 需要用户确认（当前自动放行）",
                    call.name
                );
            }
        }

        // 后台任务管理
        if let Some(result) = self.handle_background_task(call) {
            return result;
        }
        // Skill 详情查询
        if call.name == "get_skill_detail" {
            return self.handle_get_skill_detail(call);
        }

        // 多媒体工具
        if call.name == "generate_image" {
            return self.handle_media_generation(call, ModelCapability::ImageGeneration);
        }
        if call.name == "text_to_speech" {
            return self.handle_tts(call);
        }
        if call.name == "speech_to_text" {
            return self.handle_stt(call);
        }
        // 检查是否是 MCP 工具
        if let Some(target) = mcp_targets.get(&call.name) {
            return match execute_mcp_tool_call(call, target, mcp_config) {
                Ok(result) => result,
                Err(err) => ToolResult {
                    ok: false,
                    summary: format!("MCP工具调用失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: None,
                },
            };
        }

        // 检查 run_command 是否误用了 MCP 工具名
        if (call.name == "run_command" || call.name == "run_shell")
            && let Some((target, args)) =
                resolve_mcp_tool_call_from_run_command(call, mcp_targets, mcp_config)
        {
            return match crate::agents::execution_mcp_agent::execute_mcp_tool_call_with_args(
                &target, args, mcp_config,
            ) {
                Ok(result) => result,
                Err(err) => ToolResult {
                    ok: false,
                    summary: format!("MCP工具调用失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: None,
                },
            };
        }

        // 本地工具
        match build_tool_call_from_function(call) {
            Ok(tool_call) => match self.tool_executor.execute(&tool_call) {
                Ok(result) => result,
                Err(err) => ToolResult {
                    ok: false,
                    summary: format!("工具执行失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: None,
                },
            },
            Err(err) => ToolResult {
                ok: false,
                summary: format!("工具调用解析失败：{err}"),
                stdout: String::new(),
                stderr: err.to_string(),
                exit_code: 1,
                execution: None,
            },
        }
    }

    /// 处理后台任务工具调用
    fn handle_background_task(&self, call: &ModelFunctionCall) -> Option<ToolResult> {
        use crate::tool::background_task::{TaskStatus, task_registry};

        match call.name.as_str() {
            "spawn_task" => {
                let name = call
                    .arguments
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("task")
                    .to_string();
                let cmd = call
                    .arguments
                    .get("cmd")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let args: Vec<String> = call
                    .arguments
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                let cwd = call
                    .arguments
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if cmd.is_empty() {
                    return Some(ToolResult {
                        ok: false,
                        summary: "缺少 cmd 参数".to_string(),
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: 1,
                        execution: None,
                    });
                }

                let env = self
                    .tool_executor
                    .runtime_env()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();

                match task_registry().lock() {
                    Ok(mut reg) => match reg.spawn(name, cmd, args, cwd, env) {
                        Ok(task_id) => Some(ToolResult {
                            ok: true,
                            summary: format!("后台任务已启动，task_id={task_id}"),
                            stdout: serde_json::json!({"task_id": task_id}).to_string(),
                            stderr: String::new(),
                            exit_code: 0,
                            execution: None,
                        }),
                        Err(e) => Some(ToolResult {
                            ok: false,
                            summary: format!("启动后台任务失败：{e}"),
                            stdout: String::new(),
                            stderr: e,
                            exit_code: 1,
                            execution: None,
                        }),
                    },
                    Err(e) => Some(ToolResult {
                        ok: false,
                        summary: format!("任务注册表锁失败：{e}"),
                        stdout: String::new(),
                        stderr: e.to_string(),
                        exit_code: 1,
                        execution: None,
                    }),
                }
            }
            "query_task" => {
                let task_id = call
                    .arguments
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match task_registry().lock() {
                    Ok(mut reg) => match reg.query(task_id) {
                        Some(info) => {
                            let status_text = match &info.status {
                                TaskStatus::Running => "running".to_string(),
                                TaskStatus::Completed { exit_code } => {
                                    format!("completed (exit_code={exit_code})")
                                }
                                TaskStatus::Failed { error } => format!("failed: {error}"),
                                TaskStatus::Cancelled => "cancelled".to_string(),
                            };
                            Some(ToolResult {
                                ok: true,
                                summary: format!("任务 {} 状态：{}", info.name, status_text),
                                stdout: serde_json::to_string_pretty(&info).unwrap_or_default(),
                                stderr: String::new(),
                                exit_code: 0,
                                execution: None,
                            })
                        }
                        None => Some(ToolResult {
                            ok: false,
                            summary: format!("未找到任务：{task_id}"),
                            stdout: String::new(),
                            stderr: String::new(),
                            exit_code: 1,
                            execution: None,
                        }),
                    },
                    Err(e) => Some(ToolResult {
                        ok: false,
                        summary: e.to_string(),
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: 1,
                        execution: None,
                    }),
                }
            }
            "list_tasks" => match task_registry().lock() {
                Ok(mut reg) => {
                    let tasks = reg.list();
                    Some(ToolResult {
                        ok: true,
                        summary: format!("{} 个后台任务", tasks.len()),
                        stdout: serde_json::to_string_pretty(&tasks).unwrap_or_default(),
                        stderr: String::new(),
                        exit_code: 0,
                        execution: None,
                    })
                }
                Err(e) => Some(ToolResult {
                    ok: false,
                    summary: e.to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 1,
                    execution: None,
                }),
            },
            "cancel_task" => {
                let task_id = call
                    .arguments
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match task_registry().lock() {
                    Ok(mut reg) => match reg.cancel(task_id) {
                        Some(info) => Some(ToolResult {
                            ok: true,
                            summary: format!("任务 {} 已取消", info.name),
                            stdout: serde_json::to_string_pretty(&info).unwrap_or_default(),
                            stderr: String::new(),
                            exit_code: 0,
                            execution: None,
                        }),
                        None => Some(ToolResult {
                            ok: false,
                            summary: format!("未找到任务：{task_id}"),
                            stdout: String::new(),
                            stderr: String::new(),
                            exit_code: 1,
                            execution: None,
                        }),
                    },
                    Err(e) => Some(ToolResult {
                        ok: false,
                        summary: e.to_string(),
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: 1,
                        execution: None,
                    }),
                }
            }
            "wait_tasks" => {
                let task_ids: Vec<String> = call
                    .arguments
                    .get("task_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                let timeout_ms = call
                    .arguments
                    .get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                if task_ids.is_empty() {
                    return Some(ToolResult {
                        ok: false,
                        summary: "缺少 task_ids 参数".to_string(),
                        stdout: String::new(),
                        stderr: String::new(),
                        exit_code: 1,
                        execution: None,
                    });
                }

                let results = crate::tool::background_task::wait_tasks(task_ids, timeout_ms);
                let all_ok = results.iter().all(
                    |r| matches!(r.status, TaskStatus::Completed { exit_code } if exit_code == 0),
                );
                let running_count = results
                    .iter()
                    .filter(|r| matches!(r.status, TaskStatus::Running))
                    .count();
                let summary = if running_count > 0 {
                    format!(
                        "{} 个任务完成，{} 个仍在运行（超时）",
                        results.len() - running_count,
                        running_count
                    )
                } else {
                    format!("{} 个任务全部完成", results.len())
                };

                Some(ToolResult {
                    ok: all_ok,
                    summary,
                    stdout: serde_json::to_string_pretty(&results).unwrap_or_default(),
                    stderr: String::new(),
                    exit_code: if all_ok { 0 } else { 1 },
                    execution: None,
                })
            }
            _ => None, // 不是后台任务工具
        }
    }

    /// 处理 MCP/Skill 管理工具调用（Sender 版本，供 TurnRunner 使用）
    pub fn handle_management_tool(
        call: &ModelFunctionCall,
        tx: &Sender<TurnEvent>,
    ) -> Option<ToolResult> {
        let cmd = Self::parse_management_command(call)?;
        let desc = Self::describe_management_command(&cmd);
        let _ = tx.send(TurnEvent::ManagementCommand(cmd));
        Some(ToolResult {
            ok: true,
            summary: format!("{desc}，操作已提交"),
            stdout: format!("{desc}，将在当前执行完成后生效"),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    /// 解析管理命令
    fn parse_management_command(call: &ModelFunctionCall) -> Option<ManagementCommand> {
        match call.name.as_str() {
            "register_mcp_server" => {
                let name = call
                    .arguments
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let command = call
                    .arguments
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if name.is_empty() || command.is_empty() {
                    return None;
                }
                let args: Vec<String> = call
                    .arguments
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                let env: Vec<(String, String)> = call
                    .arguments
                    .get("env")
                    .and_then(|v| v.as_object())
                    .map(|obj| {
                        obj.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let transport = call
                    .arguments
                    .get("transport")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let endpoint = call
                    .arguments
                    .get("endpoint")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                Some(ManagementCommand::RegisterMcpServer {
                    name,
                    command,
                    args,
                    env,
                    transport,
                    endpoint,
                })
            }
            "remove_mcp_server" => Some(ManagementCommand::RemoveMcpServer {
                name: call
                    .arguments
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            }),
            "set_mcp_enabled" => Some(ManagementCommand::SetMcpServerEnabled {
                name: call
                    .arguments
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                enabled: call
                    .arguments
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            }),
            "install_skill" => {
                let path = call
                    .arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if path.is_empty() {
                    return None;
                }
                Some(ManagementCommand::InstallSkill {
                    path,
                    enabled: call
                        .arguments
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                })
            }
            "remove_skill" => Some(ManagementCommand::RemoveSkill {
                id: call
                    .arguments
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            }),
            "set_skill_enabled" => Some(ManagementCommand::SetSkillEnabled {
                id: call
                    .arguments
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                enabled: call
                    .arguments
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            }),
            _ => None,
        }
    }

    fn describe_management_command(cmd: &ManagementCommand) -> String {
        match cmd {
            ManagementCommand::RegisterMcpServer { name, .. } => format!("注册 MCP 服务器：{name}"),
            ManagementCommand::RemoveMcpServer { name } => format!("移除 MCP 服务器：{name}"),
            ManagementCommand::SetMcpServerEnabled { name, enabled } => format!(
                "{}MCP 服务器：{name}",
                if *enabled { "启用" } else { "禁用" }
            ),
            ManagementCommand::InstallSkill { path, .. } => format!("安装 Skill：{path}"),
            ManagementCommand::RemoveSkill { id } => format!("卸载 Skill：{id}"),
            ManagementCommand::SetSkillEnabled { id, enabled } => {
                format!("{}Skill：{id}", if *enabled { "启用" } else { "禁用" })
            }
        }
    }

    fn handle_get_skill_detail(&self, call: &ModelFunctionCall) -> ToolResult {
        let skill_id = call
            .arguments
            .get("skill_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let skill = self
            .agent_config
            .skills
            .installed
            .iter()
            .find(|s| s.id == skill_id && s.enabled);

        let Some(skill) = skill else {
            return ToolResult {
                ok: false,
                summary: format!("未找到 skill：{skill_id}"),
                stdout: String::new(),
                stderr: format!(
                    "可用的 skill：{}",
                    self.agent_config
                        .skills
                        .installed
                        .iter()
                        .filter(|s| s.enabled)
                        .map(|s| s.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                exit_code: 1,
                execution: None,
            };
        };

        let skill_dir = &skill.source.value;
        let skill_md = std::path::Path::new(skill_dir).join(&skill.entry);
        match std::fs::read_to_string(&skill_md) {
            Ok(content) => {
                let resolved = content.replace("{skill_dir}", skill_dir);
                ToolResult {
                    ok: true,
                    summary: format!("Skill {} 的使用说明", skill.name),
                    stdout: resolved,
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                }
            }
            Err(e) => ToolResult {
                ok: false,
                summary: format!("读取 skill 说明失败：{e}"),
                stdout: String::new(),
                stderr: e.to_string(),
                exit_code: 1,
                execution: None,
            },
        }
    }

    /// 处理多媒体生成工具调用（图片/视频）
    fn handle_media_generation(
        &self,
        call: &ModelFunctionCall,
        capability: ModelCapability,
    ) -> ToolResult {
        let prompt = call
            .arguments
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if prompt.is_empty() {
            return ToolResult {
                ok: false,
                summary: "缺少 prompt 参数".to_string(),
                stdout: String::new(),
                stderr: "prompt 不能为空".to_string(),
                exit_code: 1,
                execution: None,
            };
        }

        let resolved = match self.models_config.resolve_for_capability(capability) {
            Some(r) => r,
            None => {
                return ToolResult {
                    ok: false,
                    summary: format!("{}能力未配置", capability.display_name()),
                    stdout: String::new(),
                    stderr: "请在设置中配置对应的模型和路由".to_string(),
                    exit_code: 1,
                    execution: None,
                };
            }
        };

        let started = std::time::Instant::now();
        let tool_name = call.name.clone();

        // 使用 OpenAI 兼容 API 生成
        match capability {
            ModelCapability::ImageGeneration => {
                use tiangong_media::image::ImageGenerator;
                let generator = tiangong_media::openai_image::OpenAIImageGenerator::new(
                    resolved.api_key.clone(),
                    resolved.base_url.clone(),
                    resolved.model.clone(),
                );
                let width = call
                    .arguments
                    .get("width")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let height = call
                    .arguments
                    .get("height")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let style = call
                    .arguments
                    .get("style")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let request = tiangong_media::image::ImageGenRequest {
                    prompt,
                    negative_prompt: None,
                    width,
                    height,
                    model: Some(resolved.model.clone()),
                    style,
                    num_images: 1,
                };
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        return ToolResult {
                            ok: false,
                            summary: format!("运行时初始化失败：{e}"),
                            stdout: String::new(),
                            stderr: e.to_string(),
                            exit_code: 1,
                            execution: None,
                        };
                    }
                };
                let result = runtime.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        generator.generate(request),
                    )
                    .await
                });
                let duration_ms = started.elapsed().as_millis() as u64;
                match result {
                    Ok(Ok(resp)) => {
                        let mut parts = Vec::new();
                        for (i, img) in resp.images.iter().enumerate() {
                            if let Some(url) = &img.url {
                                parts.push(format!("![图片 {}]({})", i + 1, url));
                            } else if let Some(b64) = &img.b64_data {
                                parts.push(format!(
                                    "![图片 {}](data:image/png;base64,{})",
                                    i + 1,
                                    b64
                                ));
                            }
                        }
                        let output = parts.join("\n");
                        ToolResult {
                            ok: true,
                            summary: format!("图片生成成功（模型：{}）", resolved.model),
                            stdout: output,
                            stderr: String::new(),
                            exit_code: 0,
                            execution: Some(ToolExecutionRecord {
                                tool_name,
                                args: vec![],
                                duration_ms,
                                ok: true,
                                exit_code: 0,
                                summary: format!("图片生成成功（模型：{}）", resolved.model),
                            }),
                        }
                    }
                    Ok(Err(e)) => ToolResult {
                        ok: false,
                        summary: format!("图片生成失败：{e}"),
                        stdout: String::new(),
                        stderr: e.to_string(),
                        exit_code: 1,
                        execution: Some(ToolExecutionRecord {
                            tool_name,
                            args: vec![],
                            duration_ms,
                            ok: false,
                            exit_code: 1,
                            summary: format!("图片生成失败：{e}"),
                        }),
                    },
                    Err(_) => ToolResult {
                        ok: false,
                        summary: "图片生成超时（120秒）".to_string(),
                        stdout: String::new(),
                        stderr: "timeout".to_string(),
                        exit_code: 1,
                        execution: Some(ToolExecutionRecord {
                            tool_name,
                            args: vec![],
                            duration_ms,
                            ok: false,
                            exit_code: 1,
                            summary: "图片生成超时".to_string(),
                        }),
                    },
                }
            }
            // 视频生成暂通过 skill 机制处理
            ModelCapability::VideoGeneration => ToolResult {
                ok: false,
                summary: "视频生成请通过 skill 调用".to_string(),
                stdout: String::new(),
                stderr: "视频生成功能暂未内置，请安装对应的视频生成 skill".to_string(),
                exit_code: 1,
                execution: None,
            },
            _ => ToolResult {
                ok: false,
                summary: format!("不支持的多媒体能力：{}", capability.display_name()),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            },
        }
    }

    /// 处理语音合成（TTS）工具调用
    fn handle_tts(&self, call: &ModelFunctionCall) -> ToolResult {
        let text = call
            .arguments
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if text.is_empty() {
            return ToolResult {
                ok: false,
                summary: "缺少 text 参数".to_string(),
                stdout: String::new(),
                stderr: "text 不能为空".to_string(),
                exit_code: 1,
                execution: None,
            };
        }

        let resolved = match self
            .models_config
            .resolve_for_capability(ModelCapability::Tts)
        {
            Some(r) => r,
            None => {
                return ToolResult {
                    ok: false,
                    summary: "TTS 能力未配置".to_string(),
                    stdout: String::new(),
                    stderr: "请在设置中配置 TTS 模型路由".to_string(),
                    exit_code: 1,
                    execution: None,
                };
            }
        };

        let voice = call
            .arguments
            .get("voice")
            .and_then(|v| v.as_str())
            .map(String::from);
        let speed = call.arguments.get("speed").and_then(|v| v.as_f64());
        let output_path = call
            .arguments
            .get("output_path")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                let dir = std::path::PathBuf::from(home)
                    .join(".tiangong")
                    .join("media");
                let _ = std::fs::create_dir_all(&dir);
                dir.join(format!("tts_{}.mp3", scru128::new()))
                    .to_string_lossy()
                    .to_string()
            });

        let started = std::time::Instant::now();
        let tool_name = call.name.clone();

        use tiangong_media::tts::SpeechSynthesizer;
        let synthesizer = tiangong_media::openai_tts::OpenAITTS::new(
            resolved.api_key.clone(),
            resolved.base_url.clone(),
        );
        let request = tiangong_media::tts::SynthesizeRequest {
            text: text.clone(),
            voice,
            speed,
            model: Some(resolved.model.clone()),
            output_format: Some("mp3".to_string()),
        };

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                return ToolResult {
                    ok: false,
                    summary: format!("运行时初始化失败：{e}"),
                    stdout: String::new(),
                    stderr: e.to_string(),
                    exit_code: 1,
                    execution: None,
                };
            }
        };

        let result: Result<Result<tiangong_media::tts::SynthesizeResponse, anyhow::Error>, _> =
            runtime.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    synthesizer.synthesize(request),
                )
                .await
            });

        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(Ok(resp)) => {
                // 写入文件
                match std::fs::write(&output_path, &resp.audio) {
                    Ok(_) => {
                        let duration_info = resp
                            .duration
                            .map(|d| format!("，时长 {:.1}s", d))
                            .unwrap_or_default();
                        ToolResult {
                            ok: true,
                            summary: format!(
                                "语音合成成功（模型：{}{}）",
                                resolved.model, duration_info
                            ),
                            stdout: format!("音频文件已保存到：{output_path}"),
                            stderr: String::new(),
                            exit_code: 0,
                            execution: Some(ToolExecutionRecord {
                                tool_name,
                                args: vec![],
                                duration_ms,
                                ok: true,
                                exit_code: 0,
                                summary: format!("语音合成成功（模型：{}）", resolved.model),
                            }),
                        }
                    }
                    Err(e) => ToolResult {
                        ok: false,
                        summary: format!("音频文件写入失败：{e}"),
                        stdout: String::new(),
                        stderr: e.to_string(),
                        exit_code: 1,
                        execution: None,
                    },
                }
            }
            Ok(Err(e)) => ToolResult {
                ok: false,
                summary: format!("语音合成失败：{e}"),
                stdout: String::new(),
                stderr: e.to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name,
                    args: vec![],
                    duration_ms,
                    ok: false,
                    exit_code: 1,
                    summary: format!("语音合成失败：{e}"),
                }),
            },
            Err(_) => ToolResult {
                ok: false,
                summary: "语音合成超时（60秒）".to_string(),
                stdout: String::new(),
                stderr: "timeout".to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name,
                    args: vec![],
                    duration_ms,
                    ok: false,
                    exit_code: 1,
                    summary: "语音合成超时".to_string(),
                }),
            },
        }
    }

    /// 处理语音识别（STT）工具调用
    fn handle_stt(&self, call: &ModelFunctionCall) -> ToolResult {
        let file_path = call
            .arguments
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if file_path.is_empty() {
            return ToolResult {
                ok: false,
                summary: "缺少 file_path 参数".to_string(),
                stdout: String::new(),
                stderr: "file_path 不能为空".to_string(),
                exit_code: 1,
                execution: None,
            };
        }

        let resolved = match self
            .models_config
            .resolve_for_capability(ModelCapability::Stt)
        {
            Some(r) => r,
            None => {
                return ToolResult {
                    ok: false,
                    summary: "STT 能力未配置".to_string(),
                    stdout: String::new(),
                    stderr: "请在设置中配置 STT 模型路由".to_string(),
                    exit_code: 1,
                    execution: None,
                };
            }
        };

        // 读取音频文件
        let audio_data = match std::fs::read(&file_path) {
            Ok(data) => data,
            Err(e) => {
                return ToolResult {
                    ok: false,
                    summary: format!("读取音频文件失败：{e}"),
                    stdout: String::new(),
                    stderr: e.to_string(),
                    exit_code: 1,
                    execution: None,
                };
            }
        };

        // 根据扩展名推断 MIME 类型
        let mime_type = match std::path::Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
        {
            Some("mp3") => "audio/mpeg",
            Some("wav") => "audio/wav",
            Some("ogg") | Some("oga") => "audio/ogg",
            Some("flac") => "audio/flac",
            Some("webm") => "audio/webm",
            Some("m4a") => "audio/mp4",
            _ => "audio/mpeg",
        }
        .to_string();

        let language = call
            .arguments
            .get("language")
            .and_then(|v| v.as_str())
            .map(String::from);

        let started = std::time::Instant::now();
        let tool_name = call.name.clone();

        use tiangong_media::stt::SpeechRecognizer;
        let recognizer = tiangong_media::openai_stt::OpenAIWhisper::new(
            resolved.api_key.clone(),
            resolved.base_url.clone(),
        );
        let request = tiangong_media::stt::TranscribeRequest {
            audio: audio_data,
            mime_type,
            language,
            model: Some(resolved.model.clone()),
        };

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                return ToolResult {
                    ok: false,
                    summary: format!("运行时初始化失败：{e}"),
                    stdout: String::new(),
                    stderr: e.to_string(),
                    exit_code: 1,
                    execution: None,
                };
            }
        };

        let result: Result<Result<tiangong_media::stt::TranscribeResponse, anyhow::Error>, _> =
            runtime.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    recognizer.transcribe(request),
                )
                .await
            });

        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(Ok(resp)) => {
                let lang_info = resp
                    .language
                    .as_deref()
                    .map(|l| format!("，语言：{l}"))
                    .unwrap_or_default();
                let dur_info = resp
                    .duration
                    .map(|d| format!("，音频时长：{:.1}s", d))
                    .unwrap_or_default();
                ToolResult {
                    ok: true,
                    summary: format!(
                        "语音识别成功（模型：{}{}{dur_info}）",
                        resolved.model, lang_info
                    ),
                    stdout: resp.text,
                    stderr: String::new(),
                    exit_code: 0,
                    execution: Some(ToolExecutionRecord {
                        tool_name,
                        args: vec![],
                        duration_ms,
                        ok: true,
                        exit_code: 0,
                        summary: format!("语音识别成功（模型：{}）", resolved.model),
                    }),
                }
            }
            Ok(Err(e)) => ToolResult {
                ok: false,
                summary: format!("语音识别失败：{e}"),
                stdout: String::new(),
                stderr: e.to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name,
                    args: vec![],
                    duration_ms,
                    ok: false,
                    exit_code: 1,
                    summary: format!("语音识别失败：{e}"),
                }),
            },
            Err(_) => ToolResult {
                ok: false,
                summary: "语音识别超时（120秒）".to_string(),
                stdout: String::new(),
                stderr: "timeout".to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name,
                    args: vec![],
                    duration_ms,
                    ok: false,
                    exit_code: 1,
                    summary: "语音识别超时".to_string(),
                }),
            },
        }
    }

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
    }
}

/// 注入增强工具定义（多媒体、Skill、后台任务、MCP 管理）
pub(crate) fn inject_enhanced_tools(
    tools: &mut Vec<FunctionToolSpec>,
    models_config: &ModelsConfig,
    agent_config: &AgentConfig,
) {
    use crate::models_config::ModelCapability;
    if models_config
        .resolve_for_capability(ModelCapability::ImageGeneration)
        .is_some()
    {
        tools.push(FunctionToolSpec {
            name: "generate_image".to_string(),
            description: "根据文字描述生成图片".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "图片描述" },
                    "width": { "type": "integer", "description": "宽度（可选）" },
                    "height": { "type": "integer", "description": "高度（可选）" },
                    "style": { "type": "string", "description": "风格（可选）" }
                },
                "required": ["prompt"]
            }),
        });
    }
    if models_config
        .resolve_for_capability(ModelCapability::Tts)
        .is_some()
    {
        tools.push(FunctionToolSpec {
            name: "text_to_speech".to_string(),
            description: "将文本转换为语音音频文件".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "待合成文本" },
                    "voice": { "type": "string", "description": "音色（可选）" },
                    "speed": { "type": "number", "description": "语速（可选）" },
                    "output_path": { "type": "string", "description": "输出路径（可选）" }
                },
                "required": ["text"]
            }),
        });
    }
    if models_config
        .resolve_for_capability(ModelCapability::Stt)
        .is_some()
    {
        tools.push(FunctionToolSpec {
            name: "speech_to_text".to_string(),
            description: "将音频文件转录为文本".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "音频文件路径" },
                    "language": { "type": "string", "description": "语言提示（可选）" }
                },
                "required": ["file_path"]
            }),
        });
    }
    if agent_config.skills.installed.iter().any(|s| s.enabled) {
        tools.push(FunctionToolSpec {
            name: "get_skill_detail".to_string(),
            description: "获取已安装 Skill 的完整使用说明".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {"skill_id": {"type": "string"}}, "required": ["skill_id"]}),
        });
    }
    // 后台任务管理
    for spec in [
        (
            "spawn_task",
            "在后台启动长时间运行的命令",
            serde_json::json!({"type":"object","properties":{"name":{"type":"string"},"cmd":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"}},"required":["name","cmd"]}),
        ),
        (
            "query_task",
            "查询后台任务状态",
            serde_json::json!({"type":"object","properties":{"task_id":{"type":"string"}},"required":["task_id"]}),
        ),
        (
            "list_tasks",
            "列出所有后台任务",
            serde_json::json!({"type":"object","properties":{}}),
        ),
        (
            "cancel_task",
            "取消后台任务",
            serde_json::json!({"type":"object","properties":{"task_id":{"type":"string"}},"required":["task_id"]}),
        ),
        (
            "wait_tasks",
            "等待多个后台任务完成",
            serde_json::json!({"type":"object","properties":{"task_ids":{"type":"array","items":{"type":"string"}},"timeout_ms":{"type":"integer"}},"required":["task_ids"]}),
        ),
    ] {
        tools.push(FunctionToolSpec {
            name: spec.0.to_string(),
            description: spec.1.to_string(),
            parameters: spec.2,
        });
    }
    // MCP/Skill 管理
    for spec in [
        (
            "register_mcp_server",
            "注册 MCP 服务器",
            serde_json::json!({"type":"object","properties":{"name":{"type":"string"},"command":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"env":{"type":"object"},"transport":{"type":"string"},"endpoint":{"type":"string"}},"required":["name","command"]}),
        ),
        (
            "remove_mcp_server",
            "移除 MCP 服务器",
            serde_json::json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}),
        ),
        (
            "set_mcp_enabled",
            "启用/禁用 MCP 服务器",
            serde_json::json!({"type":"object","properties":{"name":{"type":"string"},"enabled":{"type":"boolean"}},"required":["name","enabled"]}),
        ),
        (
            "install_skill",
            "安装 Skill",
            serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"enabled":{"type":"boolean"}},"required":["path"]}),
        ),
        (
            "remove_skill",
            "卸载 Skill",
            serde_json::json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}),
        ),
        (
            "set_skill_enabled",
            "启用/禁用 Skill",
            serde_json::json!({"type":"object","properties":{"id":{"type":"string"},"enabled":{"type":"boolean"}},"required":["id","enabled"]}),
        ),
    ] {
        tools.push(FunctionToolSpec {
            name: spec.0.to_string(),
            description: spec.1.to_string(),
            parameters: spec.2,
        });
    }
}

/// 构建 ReAct agent 的系统 prompt
pub(crate) fn build_react_system_prompt(
    user_input: &str,
    models_config: &ModelsConfig,
    agent_config: &AgentConfig,
) -> String {
    use crate::models_config::ModelCapability;

    // 构建多媒体能力提示
    let mut media_hints = Vec::new();
    for cap in ModelCapability::media_capabilities() {
        if let Some(resolved) = models_config.resolve_for_capability(*cap) {
            media_hints.push(format!(
                "- {}：已配置（模型：{}）",
                cap.display_name(),
                resolved.model
            ));
        }
    }
    let media_section = if media_hints.is_empty() {
        String::new()
    } else {
        format!("\n\n已配置的多媒体能力：\n{}", media_hints.join("\n"))
    };

    // 构建已安装 skill 摘要（仅名称+描述，不注入完整 SKILL.md）
    let mut skill_summaries = Vec::new();
    for skill in &agent_config.skills.installed {
        if !skill.enabled {
            continue;
        }
        skill_summaries.push(format!(
            "- {} (id={}): {}",
            skill.name,
            skill.id,
            if skill.description.is_empty() {
                "无描述"
            } else {
                &skill.description
            }
        ));
    }
    let skills_section = if skill_summaries.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n已安装的 Skills（使用前先调用 get_skill_detail 获取完整说明）：\n{}",
            skill_summaries.join("\n")
        )
    };

    format!(
"你是天工智能助手。你可以直接回答用户问题，也可以使用工具来完成任务。

规则：
1. 如果能直接回答（闲聊、知识问答等），直接回复，不要调用工具。
2. 如果需要文件操作、代码搜索、命令执行等，调用对应的工具。
3. 每次工具调用后会收到执行结果，根据结果决定下一步：继续调用工具或给出最终回复。
4. 回复时语言简洁，直接回答问题，不要说\"让我查看\"之类的过渡语。
5. 不要在回复中包含工具调用的原始痕迹（如 ok=、exit_code= 等元数据）。
6. 回复使用 Markdown 格式：代码和命令用代码块（```语言 ... ```）包裹，使用标题、列表等结构化排版。
7. 工具调用失败时必须如实告知用户失败原因，绝对不能虚构成功结果。
8. 如果已安装的 Skill 能处理用户请求，优先通过 run_command 调用 Skill 脚本。
9. 耗时较长的命令（编译、下载、视频生成等）使用 spawn_task 在后台执行。
10. 多个可并行的耗时任务使用 spawn+join 模式：先多次调用 spawn_task 启动所有任务，再调用 wait_tasks 一次性等待全部完成。
11. 独立的后台任务（不需要等待结果的）用 spawn_task 启动后直接继续，无需 wait_tasks。{media_section}{skills_section}

用户输入：
{user_input}"
    )
}

pub(crate) fn use_stream_mode() -> bool {
    match std::env::var("API_STREAM") {
        Ok(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "0" | "false" | "off" | "no")
        }
        Err(_) => true,
    }
}

/// 清理 LLM 响应中混入的工具执行 trace 文本
pub(crate) fn strip_tool_traces_from_response(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_trace_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("工具执行") && trimmed.contains('[') && trimmed.contains(']') {
            in_trace_block = true;
            continue;
        }

        if in_trace_block {
            if trimmed.starts_with("命令:")
                || trimmed.starts_with("ok=")
                || trimmed.starts_with("summary:")
                || trimmed.starts_with("tool=")
                || trimmed.starts_with("stdout:")
                || trimmed.starts_with("stderr:")
                || trimmed.starts_with("duration_ms=")
                || trimmed.starts_with("exit_code=")
                || (trimmed.contains("ok=") && trimmed.contains("exit_code="))
            {
                continue;
            }
            if trimmed.is_empty() {
                continue;
            }
            in_trace_block = false;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if trimmed.contains("工具执行")
            && trimmed.contains('[')
            && (trimmed.contains("ok=") || trimmed.contains("exit_code="))
        {
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    let mut cleaned = String::with_capacity(result.len());
    let mut prev_empty = false;
    for line in result.lines() {
        if line.trim().is_empty() {
            if !prev_empty {
                cleaned.push('\n');
            }
            prev_empty = true;
        } else {
            cleaned.push_str(line);
            cleaned.push('\n');
            prev_empty = false;
        }
    }

    cleaned.trim().to_string()
}
