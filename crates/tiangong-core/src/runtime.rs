use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::agent_config::{AgentConfig, McpConfig};
use crate::agents::execution_mcp_agent::{
    McpFunctionTarget, execute_mcp_tool_call, resolve_mcp_tool_call_from_run_command,
};
use crate::agents::execution_tool_agent::build_tool_call_from_function;
use crate::app_state::ManagementCommand;
use crate::browser_trait::PageFetcher;
use crate::model::ToolSpec;
use crate::model::{ModelClient, SingleProviderClient, TokenUsage, ToolCall};
use crate::models_config::{ModelCapability, ModelsConfig};
use crate::planner::TaskPlan;
use crate::tool::{LocalToolExecutor, ToolExecutionRecord, ToolExecutor, ToolResult};
use crate::tool_override::ToolOverrideHandler;

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

#[derive(Clone)]
pub struct RuntimeEngine {
    client: SingleProviderClient,
    /// 轻量级文本模型客户端（标题生成等简单任务，未配置时为 None，回退到 client）
    lite_client: Option<SingleProviderClient>,
    /// 多模态模型客户端（由主模型通过附件解析工具按需调用）
    multimodal_client: Option<SingleProviderClient>,
    tool_executor: Arc<Mutex<LocalToolExecutor>>,
    pub context_limit: usize,
    agent_config: AgentConfig,
    models_config: ModelsConfig,
    core_config: Option<crate::core_config::CoreConfig>,
    permission_gate: crate::permission::PermissionGate,
    /// 浏览器页面获取能力（GUI 模式下注入）
    page_fetcher: Arc<Mutex<Option<Arc<dyn PageFetcher>>>>,
    /// 终端能力（GUI 模式下注入）
    terminal_provider: Arc<Mutex<Option<Arc<dyn crate::terminal_trait::TerminalProvider>>>>,
    /// 工具覆盖处理器（替代硬编码的工具名拦截）
    tool_overrides: Arc<Mutex<HashMap<String, Arc<dyn ToolOverrideHandler>>>>,
    /// Plugin 注册的工具规格提供者
    tool_spec_providers: Arc<Mutex<Vec<Arc<dyn crate::tool_override::ToolSpecProvider>>>>,
    /// Plugin 注册的 Prompt 段落提供者
    prompt_section_providers: Arc<Mutex<Vec<Arc<dyn crate::tool_override::PromptSectionProvider>>>>,
}

impl std::fmt::Debug for RuntimeEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeEngine")
            .field("client", &self.client)
            .field("context_limit", &self.context_limit)
            .field(
                "page_fetcher",
                &self
                    .page_fetcher
                    .lock()
                    .map(|g| g.is_some())
                    .unwrap_or(false),
            )
            .field(
                "tool_overrides",
                &self.tool_overrides.lock().map(|g| g.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
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
            lite_client: None,
            multimodal_client: None,
            tool_executor: Arc::new(Mutex::new(LocalToolExecutor::from_agent_config(
                &agent_config,
            ))),
            context_limit,
            agent_config,
            models_config: ModelsConfig::default(),
            core_config: None,
            permission_gate,
            page_fetcher: Arc::new(Mutex::new(None)),
            terminal_provider: Arc::new(Mutex::new(None)),
            tool_overrides: Arc::new(Mutex::new(HashMap::new())),
            tool_spec_providers: Arc::new(Mutex::new(Vec::new())),
            prompt_section_providers: Arc::new(Mutex::new(Vec::new())),
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
            lite_client: None,
            multimodal_client: None,
            tool_executor: Arc::new(Mutex::new(
                LocalToolExecutor::from_agent_config(&agent_config)
                    .with_shared_trust_mode(shared_trust_mode),
            )),
            context_limit,
            agent_config,
            models_config: ModelsConfig::default(),
            core_config: None,
            permission_gate,
            page_fetcher: Arc::new(Mutex::new(None)),
            terminal_provider: Arc::new(Mutex::new(None)),
            tool_overrides: Arc::new(Mutex::new(HashMap::new())),
            tool_spec_providers: Arc::new(Mutex::new(Vec::new())),
            prompt_section_providers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 设置轻量级文本模型客户端
    pub fn with_lite_client(mut self, client: SingleProviderClient) -> Self {
        self.lite_client = Some(client);
        self
    }

    pub fn with_multimodal_client(mut self, client: SingleProviderClient) -> Self {
        self.multimodal_client = Some(client);
        self
    }

    pub fn with_models_config(mut self, config: ModelsConfig) -> Self {
        self.models_config = config;
        self
    }

    pub fn with_core_config(mut self, config: crate::core_config::CoreConfig) -> Self {
        self.core_config = Some(config);
        self
    }

    /// 获取 LlmConfig 引用（优先从 core_config 取）
    pub fn llm_config(&self) -> Option<&crate::core_config::LlmConfig> {
        self.core_config.as_ref().map(|c| &c.llm)
    }

    /// 获取模型客户端引用
    pub fn client(&self) -> &SingleProviderClient {
        &self.client
    }
    /// 获取轻量级模型客户端（未配置时回退到主客户端）
    pub fn lite_client(&self) -> &SingleProviderClient {
        self.lite_client.as_ref().unwrap_or(&self.client)
    }
    pub fn multimodal_client(&self) -> &SingleProviderClient {
        self.multimodal_client.as_ref().unwrap_or(&self.client)
    }
    pub fn has_multimodal_client(&self) -> bool {
        self.multimodal_client.is_some()
    }
    /// 对话模型本身是否具备 multimodal 能力（multimodal 路由与 chat 路由指向同一模型）
    pub fn chat_is_multimodal(&self) -> bool {
        self.models_config.chat_is_multimodal()
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

    /// 注入页面获取能力（GUI 模式下由 Tauri Plugin 提供）
    pub fn set_page_fetcher(&self, fetcher: Arc<dyn PageFetcher>) {
        if let Ok(mut guard) = self.page_fetcher.lock() {
            *guard = Some(fetcher);
        }
    }

    /// 获取 page_fetcher 的克隆（用于 runtime 重建时保留）
    pub fn page_fetcher(&self) -> Option<Arc<dyn PageFetcher>> {
        self.page_fetcher.lock().ok()?.clone()
    }

    /// 注入终端能力（GUI 模式下由 Tauri Plugin 提供）
    /// 同步更新 tool_executor 使 run_command 校验后走 PTY 执行
    pub fn set_terminal_provider(
        &self,
        provider: Arc<dyn crate::terminal_trait::TerminalProvider>,
    ) {
        if let Ok(mut guard) = self.terminal_provider.lock() {
            *guard = Some(provider.clone());
        }
        if let Ok(mut guard) = self.tool_executor.lock() {
            *guard = guard.clone().with_terminal_provider(provider);
        }
    }

    /// 获取 terminal_provider 的克隆
    pub fn terminal_provider(&self) -> Option<Arc<dyn crate::terminal_trait::TerminalProvider>> {
        self.terminal_provider.lock().ok()?.clone()
    }

    /// 注册工具覆盖处理器
    pub fn register_tool_override(&self, name: &str, handler: Arc<dyn ToolOverrideHandler>) {
        if let Ok(mut guard) = self.tool_overrides.lock() {
            guard.insert(name.to_string(), handler);
        }
    }

    /// 获取所有已注册的工具覆盖（用于 runtime 重建时保留）
    pub fn tool_overrides(&self) -> HashMap<String, Arc<dyn ToolOverrideHandler>> {
        self.tool_overrides
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// 注册 Plugin 工具规格提供者
    pub fn register_tool_spec_provider(
        &self,
        provider: Arc<dyn crate::tool_override::ToolSpecProvider>,
    ) {
        if let Ok(mut guard) = self.tool_spec_providers.lock() {
            guard.push(provider);
        }
    }

    /// 收集所有 Plugin 注册的工具规格
    pub fn collect_plugin_tool_specs(&self) -> Vec<crate::model::ToolSpec> {
        self.tool_spec_providers
            .lock()
            .ok()
            .map(|guard| guard.iter().flat_map(|p| p.tool_specs()).collect())
            .unwrap_or_default()
    }

    /// 获取所有已注册的工具规格提供者（用于 runtime 重建时保留）
    pub fn tool_spec_providers(&self) -> Vec<Arc<dyn crate::tool_override::ToolSpecProvider>> {
        self.tool_spec_providers
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// 注册 Plugin Prompt 段落提供者
    pub fn register_prompt_section_provider(
        &self,
        provider: Arc<dyn crate::tool_override::PromptSectionProvider>,
    ) {
        if let Ok(mut guard) = self.prompt_section_providers.lock() {
            guard.push(provider);
        }
    }

    /// 收集所有 Plugin 注册的 Prompt 段落
    pub fn collect_plugin_prompt_sections(&self) -> Vec<String> {
        self.prompt_section_providers
            .lock()
            .ok()
            .map(|guard| guard.iter().flat_map(|p| p.prompt_sections()).collect())
            .unwrap_or_default()
    }

    /// 获取所有已注册的 Prompt 段落提供者（用于 runtime 重建时保留）
    pub fn prompt_section_providers(
        &self,
    ) -> Vec<Arc<dyn crate::tool_override::PromptSectionProvider>> {
        self.prompt_section_providers
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn provider_label(&self) -> String {
        format!(
            "{} @ {} · {}ms",
            self.client.api_model(),
            self.client.api_base_url(),
            self.client.api_timeout_ms()
        )
    }

    /// 对工具进行权限检查（暴露给 core 层在执行前调用）
    pub(crate) fn check_tool_permission(
        &self,
        tool_name: &str,
    ) -> crate::permission::PermissionDecision {
        self.permission_gate.check(tool_name)
    }

    /// 执行单个工具调用（本地工具、MCP 工具、多媒体生成或后台任务）
    ///
    /// 注意：权限检查已由调用方（core/mod.rs）在执行前完成，
    /// 此方法内部的权限检查改为仅 Denied 拦截，NeedsApproval 由调用方处理。
    pub(crate) async fn execute_tool_call(
        &self,
        call: &ToolCall,
        mcp_targets: &HashMap<String, McpFunctionTarget>,
        mcp_config: &McpConfig,
        session_id: &str,
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
            PermissionDecision::NeedsApproval { .. } => {
                // 审批已由调用方（core/mod.rs execute_turn_inner）完成，此处放行
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
            return self
                .handle_media_generation(call, ModelCapability::ImageGeneration)
                .await;
        }
        if call.name == "generate_video" {
            return self
                .handle_media_generation(call, ModelCapability::VideoGeneration)
                .await;
        }
        if call.name == "text_to_speech" {
            return self.handle_tts(call).await;
        }
        if call.name == "speech_to_text" {
            return self.handle_stt(call).await;
        }
        // 检查是否是 MCP 工具
        if let Some(target) = mcp_targets.get(&call.name) {
            return match execute_mcp_tool_call(call, target, mcp_config).await {
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
            )
            .await
            {
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

        // 工具覆盖（Plugin 注入的浏览器获取等能力优先于默认行为）
        if let Some(handler) = self
            .tool_overrides
            .lock()
            .ok()
            .and_then(|g| g.get(&call.name).cloned())
            && let Some(result) = handler.handle(call, session_id).await
        {
            return result;
        }

        // 本地工具
        let executor = self
            .tool_executor
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| LocalToolExecutor::from_agent_config(&self.agent_config));
        match build_tool_call_from_function(call) {
            Ok(tool_call) => match executor.execute(&tool_call, session_id) {
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
    fn handle_background_task(&self, call: &ToolCall) -> Option<ToolResult> {
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
                    .lock()
                    .map(|g| {
                        g.runtime_env()
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

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

    /// 解析管理命令
    #[allow(dead_code)]
    fn parse_management_command(call: &ToolCall) -> Option<ManagementCommand> {
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

    #[allow(dead_code)]
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

    fn handle_get_skill_detail(&self, call: &ToolCall) -> ToolResult {
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

        let missing_mcp = missing_managed_mcp_servers(skill, &self.agent_config);
        if !missing_mcp.is_empty() {
            return ToolResult {
                ok: false,
                summary: format!("Skill {skill_id} 缺少托管 MCP 依赖"),
                stdout: String::new(),
                stderr: format!(
                    "SkillActivationError::MissingMcp skill_id={skill_id} missing={}；请提示用户确认后补充注册这些托管 MCP",
                    missing_mcp.join(",")
                ),
                exit_code: 1,
                execution: None,
            };
        }

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

    fn media_error_summary(prefix: &str, err: &crate::media::MediaServiceError) -> String {
        if err.is_timeout() || err.is_config() {
            err.to_string()
        } else {
            format!("{prefix}失败：{err}")
        }
    }

    /// 处理多媒体生成工具调用（图片/视频）
    async fn handle_media_generation(
        &self,
        call: &ToolCall,
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

        let started = std::time::Instant::now();
        let tool_name = call.name.clone();

        // 使用 OpenAI 兼容 API 生成
        match capability {
            ModelCapability::ImageGeneration => {
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
                let result =
                    crate::media::generate_image(&self.models_config, prompt, width, height, style)
                        .await;
                let duration_ms = started.elapsed().as_millis() as u64;
                match result {
                    Ok(output) => {
                        let mut parts = Vec::new();
                        for (i, img) in output.response.images.iter().enumerate() {
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
                        let markdown = parts.join("\n");
                        ToolResult {
                            ok: true,
                            summary: format!("图片生成成功（模型：{}）", output.resolved.model),
                            stdout: markdown,
                            stderr: String::new(),
                            exit_code: 0,
                            execution: Some(ToolExecutionRecord {
                                tool_name,
                                args: vec![],
                                duration_ms,
                                ok: true,
                                exit_code: 0,
                                summary: format!("图片生成成功（模型：{}）", output.resolved.model),
                            }),
                        }
                    }
                    Err(err) => ToolResult {
                        ok: false,
                        summary: Self::media_error_summary("图片生成", &err),
                        stdout: String::new(),
                        stderr: err.to_string(),
                        exit_code: 1,
                        execution: Some(ToolExecutionRecord {
                            tool_name,
                            args: vec![],
                            duration_ms,
                            ok: false,
                            exit_code: 1,
                            summary: Self::media_error_summary("图片生成", &err),
                        }),
                    },
                }
            }
            ModelCapability::VideoGeneration => {
                let duration = call
                    .arguments
                    .get("duration")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let resolution = call
                    .arguments
                    .get("resolution")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let result =
                    crate::media::generate_video(&self.models_config, prompt, duration, resolution)
                        .await;
                let duration_ms = started.elapsed().as_millis() as u64;
                match result {
                    Ok(output) => {
                        use tiangong_media::video::VideoGenStatus;

                        let (ok, summary, stdout, stderr, exit_code) = match output.response.status
                        {
                            VideoGenStatus::Completed {
                                video_url,
                                duration,
                            } => {
                                let duration_line = duration
                                    .map(|seconds| format!("\nDuration: {seconds:.1}s"))
                                    .unwrap_or_default();
                                (
                                    true,
                                    format!("视频生成成功（模型：{}）", output.resolved.model),
                                    format!("Video URL: {video_url}{duration_line}"),
                                    String::new(),
                                    0,
                                )
                            }
                            VideoGenStatus::Pending => (
                                true,
                                format!("视频生成任务已提交（模型：{}）", output.resolved.model),
                                format!("Task ID: {}\nStatus: pending", output.response.task_id),
                                String::new(),
                                0,
                            ),
                            VideoGenStatus::Processing { progress } => {
                                let progress_line = progress
                                    .map(|p| format!("\nProgress: {p:.1}%"))
                                    .unwrap_or_default();
                                (
                                    true,
                                    format!(
                                        "视频生成任务处理中（模型：{}）",
                                        output.resolved.model
                                    ),
                                    format!(
                                        "Task ID: {}\nStatus: processing{progress_line}",
                                        output.response.task_id
                                    ),
                                    String::new(),
                                    0,
                                )
                            }
                            VideoGenStatus::Failed { error } => (
                                false,
                                format!("视频生成失败：{error}"),
                                String::new(),
                                error,
                                1,
                            ),
                        };
                        ToolResult {
                            ok,
                            summary: summary.clone(),
                            stdout,
                            stderr,
                            exit_code,
                            execution: Some(ToolExecutionRecord {
                                tool_name,
                                args: vec![],
                                duration_ms,
                                ok,
                                exit_code,
                                summary,
                            }),
                        }
                    }
                    Err(err) => ToolResult {
                        ok: false,
                        summary: Self::media_error_summary("视频生成", &err),
                        stdout: String::new(),
                        stderr: err.to_string(),
                        exit_code: 1,
                        execution: Some(ToolExecutionRecord {
                            tool_name,
                            args: vec![],
                            duration_ms,
                            ok: false,
                            exit_code: 1,
                            summary: Self::media_error_summary("视频生成", &err),
                        }),
                    },
                }
            }
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
    async fn handle_tts(&self, call: &ToolCall) -> ToolResult {
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

        let result = crate::media::synthesize_speech(
            &self.models_config,
            text.clone(),
            voice,
            speed,
            Some("mp3".to_string()),
        )
        .await;

        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(output) => {
                // 写入文件
                match std::fs::write(&output_path, &output.response.audio) {
                    Ok(_) => {
                        let duration_info = output
                            .response
                            .duration
                            .map(|d| format!("，时长 {:.1}s", d))
                            .unwrap_or_default();
                        ToolResult {
                            ok: true,
                            summary: format!(
                                "语音合成成功（模型：{}{}）",
                                output.resolved.model, duration_info
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
                                summary: format!("语音合成成功（模型：{}）", output.resolved.model),
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
            Err(err) => ToolResult {
                ok: false,
                summary: Self::media_error_summary("语音合成", &err),
                stdout: String::new(),
                stderr: err.to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name,
                    args: vec![],
                    duration_ms,
                    ok: false,
                    exit_code: 1,
                    summary: Self::media_error_summary("语音合成", &err),
                }),
            },
        }
    }

    /// 处理语音识别（STT）工具调用
    async fn handle_stt(&self, call: &ToolCall) -> ToolResult {
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

        let result =
            crate::media::transcribe_audio(&self.models_config, audio_data, mime_type, language)
                .await;

        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(output) => {
                let lang_info = output
                    .response
                    .language
                    .as_deref()
                    .map(|l| format!("，语言：{l}"))
                    .unwrap_or_default();
                let dur_info = output
                    .response
                    .duration
                    .map(|d| format!("，音频时长：{:.1}s", d))
                    .unwrap_or_default();
                ToolResult {
                    ok: true,
                    summary: format!(
                        "语音识别成功（模型：{}{}{dur_info}）",
                        output.resolved.model, lang_info
                    ),
                    stdout: output.response.text,
                    stderr: String::new(),
                    exit_code: 0,
                    execution: Some(ToolExecutionRecord {
                        tool_name,
                        args: vec![],
                        duration_ms,
                        ok: true,
                        exit_code: 0,
                        summary: format!("语音识别成功（模型：{}）", output.resolved.model),
                    }),
                }
            }
            Err(err) => ToolResult {
                ok: false,
                summary: Self::media_error_summary("语音识别", &err),
                stdout: String::new(),
                stderr: err.to_string(),
                exit_code: 1,
                execution: Some(ToolExecutionRecord {
                    tool_name,
                    args: vec![],
                    duration_ms,
                    ok: false,
                    exit_code: 1,
                    summary: Self::media_error_summary("语音识别", &err),
                }),
            },
        }
    }

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
    }
}

/// 注入增强工具定义（多媒体、Skill、后台任务、MCP 管理）
pub(crate) fn inject_enhanced_tools(tools: &mut Vec<ToolSpec>, engine: &RuntimeEngine) {
    let agent_config = engine.agent_config();

    // 多媒体能力判断：优先使用 LlmConfig（新路径），回退 ModelsConfig（旧路径）
    let has_image_gen = engine
        .llm_config()
        .map(|c| c.image_generation.is_some())
        .unwrap_or_else(|| {
            engine
                .models_config()
                .resolve_slot(crate::models_config::RoutingSlot::ImageGeneration)
                .is_some()
        });
    let has_video_gen = engine
        .llm_config()
        .map(|c| c.video_generation.is_some())
        .unwrap_or_else(|| {
            engine
                .models_config()
                .resolve_slot(crate::models_config::RoutingSlot::VideoGeneration)
                .is_some()
        });
    let has_tts = engine
        .llm_config()
        .map(|c| c.tts.is_some())
        .unwrap_or_else(|| {
            engine
                .models_config()
                .resolve_slot(crate::models_config::RoutingSlot::Tts)
                .is_some()
        });
    let has_stt = engine
        .llm_config()
        .map(|c| c.stt.is_some())
        .unwrap_or_else(|| {
            engine
                .models_config()
                .resolve_slot(crate::models_config::RoutingSlot::Stt)
                .is_some()
        });
    let has_multimodal = engine.has_multimodal_client();
    // 当对话模型本身就是 multimodal 时，图片直接随消息发送，不需要工具
    let chat_is_multimodal = engine.chat_is_multimodal();

    if has_multimodal && !chat_is_multimodal {
        tools.push(ToolSpec {
            name: "analyze_attachment".to_string(),
            description: "按需调用多模态模型解析用户上传的图片或文件附件。只有当用户问题确实需要查看附件内容时才调用；普通文本对话不要调用。重要：message_id 必须使用用户消息中提示文字所标注的 ID，不要使用其他消息的 ID。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "instruction": {
                        "type": "string",
                        "description": "希望多模态模型如何解析附件，例如提取文字、描述画面、识别表格、回答与附件有关的问题"
                    },
                    "message_id": {
                        "type": "string",
                        "description": "包含附件的用户消息 ID。省略时使用最近一条包含附件的用户消息"
                    },
                    "attachment_index": {
                        "type": "integer",
                        "description": "附件序号，从 0 开始。省略时解析该消息中的全部附件"
                    }
                },
                "required": ["instruction"]
            }),
        });
    }

    if has_image_gen {
        tools.push(ToolSpec {
            name: "generate_image".to_string(),
            description: "根据文字描述生成图片。每次调用会等待生成完成后返回图片路径。\
            注意：同一轮次中不要重复调用相同 prompt 的 generate_image，\
            拿到图片结果后应直接继续后续任务（如编写 HTML、组合排版等）。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "图片描述，建议使用英文以获得更好效果" },
                    "width": { "type": "integer", "description": "宽度（可选）" },
                    "height": { "type": "integer", "description": "高度（可选）" },
                    "style": { "type": "string", "description": "风格（可选）" }
                },
                "required": ["prompt"]
            }),
        });
    }
    if has_video_gen {
        tools.push(ToolSpec {
            name: "generate_video".to_string(),
            description: "根据文字描述生成视频，成功时返回结构化视频资源".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "视频描述" },
                    "duration": { "type": "integer", "description": "视频时长，单位秒（可选）" },
                    "resolution": { "type": "string", "description": "分辨率，如 720p、1080p（可选）" }
                },
                "required": ["prompt"]
            }),
        });
    }
    if has_tts {
        tools.push(ToolSpec {
            name: "text_to_speech".to_string(),
            description: "将文本转换为语音音频文件".to_string(),
            input_schema: serde_json::json!({
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
    if has_stt {
        tools.push(ToolSpec {
            name: "speech_to_text".to_string(),
            description: "将音频文件转录为文本".to_string(),
            input_schema: serde_json::json!({
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
        tools.push(ToolSpec {
            name: "get_skill_detail".to_string(),
            description: "获取已安装 Skill 的完整使用说明".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {"skill_id": {"type": "string"}}, "required": ["skill_id"]}),
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
        tools.push(ToolSpec {
            name: spec.0.to_string(),
            description: spec.1.to_string(),
            input_schema: spec.2,
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
        tools.push(ToolSpec {
            name: spec.0.to_string(),
            description: spec.1.to_string(),
            input_schema: spec.2,
        });
    }

    // 多智能体团队工具
    crate::agent_team::tools::inject_agent_team_tools(tools);
}

fn missing_managed_mcp_servers(
    skill: &crate::agent_config::InstalledSkillConfig,
    agent_config: &AgentConfig,
) -> Vec<String> {
    if skill.requires_mcp.is_empty() {
        return Vec::new();
    }
    let configured = agent_config
        .mcp
        .servers
        .iter()
        .map(|server| server.name.as_str())
        .collect::<HashSet<_>>();

    skill
        .requires_mcp
        .iter()
        .filter_map(|requirement| {
            let mcp_id = if requirement.id.trim().is_empty() {
                requirement.package.trim()
            } else {
                requirement.id.trim()
            };
            if mcp_id.is_empty() {
                return None;
            }
            let server_name = format!("skill::{}::{mcp_id}", skill.id);
            (!configured.contains(server_name.as_str())).then_some(server_name)
        })
        .collect()
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
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_config::{
        InstalledSkillConfig, McpServerConfig, McpTransportMode, SkillMcpRequirementConfig,
        SkillSourceConfig,
    };

    #[test]
    fn missing_managed_mcp_servers_reports_absent_skill_server() {
        let skill = InstalledSkillConfig {
            id: "demo".to_string(),
            name: "Demo".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            entry: "SKILL.md".to_string(),
            enabled: true,
            installed_at: String::new(),
            managed_mcp_servers: vec!["skill::demo::tool".to_string()],
            source: SkillSourceConfig {
                kind: "local".to_string(),
                value: "/tmp/demo".to_string(),
            },
            requires_mcp: vec![SkillMcpRequirementConfig {
                id: "tool".to_string(),
                source: "npm".to_string(),
                package: "demo-tool".to_string(),
                version: "1.0.0".to_string(),
            }],
            permissions: Default::default(),
        };

        let missing = missing_managed_mcp_servers(&skill, &AgentConfig::default());
        assert_eq!(missing, vec!["skill::demo::tool"]);
    }

    #[test]
    fn missing_managed_mcp_servers_accepts_configured_skill_server() {
        let mut config = AgentConfig::default();
        config.mcp.servers.push(McpServerConfig {
            name: "skill::demo::tool".to_string(),
            transport: McpTransportMode::Stdio,
            command: "echo".to_string(),
            args: Vec::new(),
            endpoint: String::new(),
            auth_header: String::new(),
            headers: Default::default(),
            env: Default::default(),
            cwd: String::new(),
            enabled: true,
            tags: Vec::new(),
        });
        let skill = InstalledSkillConfig {
            id: "demo".to_string(),
            name: "Demo".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            entry: "SKILL.md".to_string(),
            enabled: true,
            installed_at: String::new(),
            managed_mcp_servers: vec!["skill::demo::tool".to_string()],
            source: SkillSourceConfig {
                kind: "local".to_string(),
                value: "/tmp/demo".to_string(),
            },
            requires_mcp: vec![SkillMcpRequirementConfig {
                id: "tool".to_string(),
                source: "npm".to_string(),
                package: "demo-tool".to_string(),
                version: "1.0.0".to_string(),
            }],
            permissions: Default::default(),
        };

        let missing = missing_managed_mcp_servers(&skill, &config);
        assert!(missing.is_empty());
    }
}
