use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::agent_config::{AgentConfig, McpConfig};
use crate::agents::execution_mcp_agent::{
    McpFunctionTarget, execute_mcp_tool_call, resolve_mcp_tool_call_from_run_command,
};
use crate::app_state::ManagementCommand;
use crate::browser_trait::PageFetcher;
use crate::model::ToolSpec;
use crate::model::{ModelClient, SingleProviderClient, TokenUsage, ToolCall};
use crate::models_config::ModelsConfig;
use crate::planner::TaskPlan;
use crate::session::Session;
use crate::tool::{ToolExecutionRecord, ToolResult};
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

/// 单条验证命令的执行结果记录。
///
/// 由 ReAct 执行链路在需要时收集（如运行测试、构建等验证命令），用于
/// 上下文呈现与结果汇总。历史定义位于 `agents::response_agent`，随旧
/// 流水线退场而收敛到 `runtime` 作为通用数据类型。
#[derive(Debug, Clone)]
pub struct VerifyExecutionRecord {
    pub command: String,
    pub ok: bool,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
}

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
    /// MCP / skills 收集的环境变量（供子进程执行注入，原 LocalToolExecutor.runtime_env）
    runtime_env: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
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
            runtime_env: Arc::new(Mutex::new(crate::runtime_env::collect_runtime_env(
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
            runtime_env: Arc::new(Mutex::new(crate::runtime_env::collect_runtime_env(
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

    /// 注入终端能力（GUI 模式下由 terminal 插件提供，PTY 执行）
    pub fn set_terminal_provider(
        &self,
        provider: Arc<dyn crate::terminal_trait::TerminalProvider>,
    ) {
        if let Ok(mut guard) = self.terminal_provider.lock() {
            *guard = Some(provider);
        }
    }

    /// 获取 MCP / skills 收集的环境变量快照（供 command 插件注入子进程）。
    pub fn runtime_env(&self) -> std::collections::BTreeMap<String, String> {
        self.runtime_env
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
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
        session: &Session,
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

        // 多媒体工具（generate_image / generate_video / text_to_speech / speech_to_text）
        // 已迁移至独立插件 crate（tiangong-plugin-{generate-image,generate-video,
        // text-to-speech,speech-to-text}），由 tool_overrides 统一分发。

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

        // 工具覆盖（Plugin 注入的能力：fs / fetch / command / browser / terminal 等）
        if let Some(handler) = self
            .tool_overrides
            .lock()
            .ok()
            .and_then(|g| g.get(&call.name).cloned())
            && let Some(result) = handler.handle(call, session).await
        {
            return result;
        }

        // 无任何处理器命中的工具：LocalToolExecutor 已删除，所有工具均由插件提供。
        ToolResult {
            ok: false,
            summary: format!("未注册的工具：{}（请确认对应插件已启用）", call.name),
            stdout: String::new(),
            stderr: format!("tool {} not handled by any plugin", call.name),
            exit_code: 1,
            execution: None,
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
                    .runtime_env
                    .lock()
                    .map(|g| {
                        g.iter()
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

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
    }
}

/// 注入增强工具定义（Skill、后台任务、MCP 管理）
pub(crate) fn inject_enhanced_tools(tools: &mut Vec<ToolSpec>, engine: &RuntimeEngine) {
    let agent_config = engine.agent_config();

    // 多媒体能力（图片/视频/TTS/STT）与附件分析（analyze_attachment）的工具规格与
    // 分发已迁移至独立插件 crate（tiangong-plugin-{generate-image,generate-video,
    // text-to-speech,speech-to-text,analyze-attachment}），由 tool_overrides 统一分发。

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
            "在后台启动特殊命令。仅当用户明确要求后台、不阻塞、并行执行、持续运行、启动服务/监听，或需要让命令跨多轮继续运行时使用；普通命令、构建、检查、git、文件操作必须优先使用 run_shell 或 run_command。",
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
            "等待已通过 spawn_task 启动的后台任务完成。仅用于已有后台任务，不用于执行普通命令。",
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
