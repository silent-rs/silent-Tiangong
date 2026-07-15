use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agent_config::AgentConfig;
use crate::model::{ModelClient, SingleProviderClient, TokenUsage};
use crate::models_config::ModelsConfig;
use crate::planner::TaskPlan;
use crate::session::Session;
use crate::tool::ToolExecutionRecord;
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
    /// 各插件贡献的子进程环境变量（供子进程执行注入）
    runtime_env: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
    pub context_limit: usize,
    agent_config: AgentConfig,
    models_config: ModelsConfig,
    core_config: Option<crate::core_config::CoreConfig>,
    /// 工具覆盖处理器（替代硬编码的工具名拦截）
    tool_overrides: Arc<Mutex<HashMap<String, Arc<dyn ToolOverrideHandler>>>>,
    /// Plugin 注册的工具规格提供者
    tool_spec_providers: Arc<Mutex<Vec<Arc<dyn crate::tool_override::ToolSpecProvider>>>>,
    /// Plugin 注册的 Prompt 段落提供者
    prompt_section_providers: Arc<Mutex<Vec<Arc<dyn crate::tool_override::PromptSectionProvider>>>>,
    /// Turn-scoped 插件 usage 收集器（由插件经 PluginFeedbackTx.report_token_usage
    /// 即时累加，turn 开始绑定 / 结束解绑，见 core::plugin::feedback::TurnUsageSink）。
    turn_usage_sink: Arc<crate::core::plugin::TurnUsageSink>,
}

impl std::fmt::Debug for RuntimeEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeEngine")
            .field("client", &self.client)
            .field("context_limit", &self.context_limit)
            .field(
                "tool_overrides",
                &self.tool_overrides.lock().map(|g| g.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl RuntimeEngine {
    #[cfg(test)]
    pub(crate) fn for_react_test(client: SingleProviderClient) -> Self {
        let agent_config = AgentConfig::default();
        Self {
            client,
            lite_client: None,
            runtime_env: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            context_limit: 8_192,
            agent_config,
            models_config: ModelsConfig::default(),
            core_config: None,
            tool_overrides: Arc::new(Mutex::new(HashMap::new())),
            tool_spec_providers: Arc::new(Mutex::new(Vec::new())),
            prompt_section_providers: Arc::new(Mutex::new(Vec::new())),
            turn_usage_sink: Arc::new(crate::core::plugin::TurnUsageSink::new()),
        }
    }

    pub fn new(
        client: SingleProviderClient,
        context_limit: usize,
        agent_config: AgentConfig,
        storage_root: std::path::PathBuf,
    ) -> Self {
        crate::storage::set_storage_root(storage_root);
        Self {
            client,
            lite_client: None,
            runtime_env: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            context_limit,
            agent_config,
            models_config: ModelsConfig::default(),
            core_config: None,
            tool_overrides: Arc::new(Mutex::new(HashMap::new())),
            tool_spec_providers: Arc::new(Mutex::new(Vec::new())),
            prompt_section_providers: Arc::new(Mutex::new(Vec::new())),
            turn_usage_sink: Arc::new(crate::core::plugin::TurnUsageSink::new()),
        }
    }

    /// 使用共享的信任模式引用创建（确保跨 clone 实例共享同一权限状态）
    pub fn with_shared_trust_mode(
        client: SingleProviderClient,
        context_limit: usize,
        agent_config: AgentConfig,
        trust_mode: crate::permission::TrustMode,
        storage_root: std::path::PathBuf,
    ) -> Self {
        let _ = trust_mode;
        crate::storage::set_storage_root(storage_root);
        Self {
            client,
            lite_client: None,
            runtime_env: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            context_limit,
            agent_config,
            models_config: ModelsConfig::default(),
            core_config: None,
            tool_overrides: Arc::new(Mutex::new(HashMap::new())),
            tool_spec_providers: Arc::new(Mutex::new(Vec::new())),
            prompt_section_providers: Arc::new(Mutex::new(Vec::new())),
            turn_usage_sink: Arc::new(crate::core::plugin::TurnUsageSink::new()),
        }
    }

    /// 设置轻量级文本模型客户端
    pub fn with_lite_client(mut self, client: SingleProviderClient) -> Self {
        self.lite_client = Some(client);
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

    /// 取 turn-scoped 插件 usage 收集器的共享引用（供 ReactEngine 在 turn
    /// 开始时绑定本轮 usage 上下文，插件经 PluginFeedbackTx 即时上报）。
    pub fn turn_usage_sink(&self) -> &Arc<crate::core::plugin::TurnUsageSink> {
        &self.turn_usage_sink
    }

    /// 获取各插件贡献的环境变量快照（供 command 插件注入子进程）。
    pub fn runtime_env(&self) -> std::collections::BTreeMap<String, String> {
        self.runtime_env
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// 写入由各插件 exec_env 汇总的环境变量（供 core/mod.rs 在所有插件
    /// 注册完成后统一调用，并经 `Plugin::set_exec_env` 回注给消费方插件）。
    pub fn set_runtime_env(&self, env: std::collections::BTreeMap<String, String>) {
        if let Ok(mut guard) = self.runtime_env.lock() {
            *guard = env;
        }
    }

    /// 注册工具覆盖处理器（first-writer-wins）。
    ///
    /// 若该工具名已被其他插件注册，保留先注册者，跳过当前注册。
    /// 这保证了内置业务插件（fs/fetch/command 等）的工具优先于后注册的动态工具
    ///（如其他插件暴露的同名工具），对齐原 `execution_function_tools` 的
    /// `reserved_names` 冲突消解语义。
    pub fn register_tool_override(&self, name: &str, handler: Arc<dyn ToolOverrideHandler>) {
        if let Ok(mut guard) = self.tool_overrides.lock() {
            guard.entry(name.to_string()).or_insert(handler);
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

    pub fn fallback_error_message(err: &anyhow::Error) -> String {
        format!("执行失败：{err}")
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
