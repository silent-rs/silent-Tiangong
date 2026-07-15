//! 一轮对话的执行上下文。
//!
//! [`TurnContext`] 是 turn 级能力容器：client / 权限 / 插件注册表 / 配置 / 工具 /
//! 用量收集器。生命周期严格限制为单个 turn——收到 Message 时由 `build_turn_context`
//! 构造,turn 结束后整体销毁。不跨 turn 复用。
//!
//! 与 `react/` 模块的关系:`TurnContext` 是被 react 层消费的能力集合,本身不属于
//! ReAct 执行流程。`react/engine.rs` 在 `impl TurnContext` 上定义 `execute_turn`
//! 等 ReAct 循环方法。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::agent_config::AgentConfig;
use crate::model::{SingleProviderClient, ToolCall};
use crate::models_config::ModelsConfig;
use crate::session::Session;
use crate::tool::ToolResult;
use crate::tool_override::ToolOverrideHandler;

/// 一轮对话的执行上下文（替代原 ReactEngine + RuntimeEngine）。
///
/// 生命周期严格限制为单个 turn：收到 Message 时由 `build_turn_context` 构造，
/// turn 结束后整体销毁（含 client / 权限 / 用量收集器）。不跨 turn 复用。
#[derive(Clone)]
pub struct TurnContext {
    /// 模型请求客户端（chat 槽位）
    pub client: SingleProviderClient,
    /// 轻量级文本模型客户端（标题生成等简单任务，未配置时为 None，回退到 client）
    pub lite_client: Option<SingleProviderClient>,
    /// 上下文 token 上限
    pub context_limit: usize,
    /// Agent 配置（reasoning_effort 等）
    pub agent_config: AgentConfig,
    /// 模型配置
    pub models_config: ModelsConfig,
    /// Core 配置快照
    pub core_config: Option<crate::core_config::CoreConfig>,
    /// 权限判断器
    pub permission_gate: crate::permission::PermissionGate,
    /// 各插件贡献的子进程环境变量
    pub runtime_env: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
    /// 工具覆盖处理器（替代硬编码的工具名拦截）
    pub tool_overrides: Arc<Mutex<HashMap<String, Arc<dyn ToolOverrideHandler>>>>,
    /// Plugin 注册的工具规格提供者
    pub tool_spec_providers: Arc<Mutex<Vec<Arc<dyn crate::tool_override::ToolSpecProvider>>>>,
    /// Plugin 注册的 Prompt 段落提供者
    pub prompt_section_providers:
        Arc<Mutex<Vec<Arc<dyn crate::tool_override::PromptSectionProvider>>>>,
    /// Turn-scoped 插件 usage 收集器
    pub turn_usage_sink: Arc<crate::core::plugin::TurnUsageSink>,
    // ===== turn 级配置 =====
    /// 当前执行单元可用的工具集
    pub tools: Vec<crate::model::ToolSpec>,
    /// 单次工具执行阶段（ReAct Loop 内层）的最大轮次
    pub max_tool_rounds: usize,
    /// 总结阶段后重新进入工具执行阶段的最大次数
    pub max_outer_iterations: u32,
    /// 当前执行单元身份
    pub agent_id: String,
    /// 调用方提供的完整 system prompt（嵌套 Agent 用）
    pub system_prompt_override: Option<crate::session::Message>,
}

impl TurnContext {
    pub fn new(
        client: SingleProviderClient,
        context_limit: usize,
        agent_config: AgentConfig,
        tools: Vec<crate::model::ToolSpec>,
        max_tool_rounds: usize,
        max_outer_iterations: u32,
        permission_gate: crate::permission::PermissionGate,
        turn_usage_sink: Arc<crate::core::plugin::TurnUsageSink>,
    ) -> Self {
        Self {
            client,
            lite_client: None,
            context_limit,
            agent_config,
            models_config: ModelsConfig::default(),
            core_config: None,
            permission_gate,
            runtime_env: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            tool_overrides: Arc::new(Mutex::new(HashMap::new())),
            tool_spec_providers: Arc::new(Mutex::new(Vec::new())),
            prompt_section_providers: Arc::new(Mutex::new(Vec::new())),
            turn_usage_sink,
            tools,
            max_tool_rounds,
            max_outer_iterations,
            agent_id: "main".to_string(),
            system_prompt_override: None,
        }
    }

    pub fn with_agent_id(mut self, agent_id: String) -> Self {
        self.agent_id = agent_id;
        self
    }

    pub fn with_system_prompt(mut self, system_prompt: crate::session::Message) -> Self {
        self.system_prompt_override = Some(system_prompt);
        self
    }

    // ===== 能力 accessor =====

    pub fn client(&self) -> &SingleProviderClient {
        &self.client
    }

    pub fn lite_client(&self) -> &SingleProviderClient {
        self.lite_client.as_ref().unwrap_or(&self.client)
    }

    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent_config
    }

    pub fn models_config(&self) -> &ModelsConfig {
        &self.models_config
    }

    pub fn permission_gate(&self) -> &crate::permission::PermissionGate {
        &self.permission_gate
    }

    pub fn turn_usage_sink(&self) -> &Arc<crate::core::plugin::TurnUsageSink> {
        &self.turn_usage_sink
    }

    pub fn runtime_env(&self) -> std::collections::BTreeMap<String, String> {
        self.runtime_env
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub fn set_runtime_env(&self, env: std::collections::BTreeMap<String, String>) {
        if let Ok(mut guard) = self.runtime_env.lock() {
            *guard = env;
        }
    }

    // ===== 插件注册 =====

    pub fn register_tool_override(&self, name: &str, handler: Arc<dyn ToolOverrideHandler>) {
        let mut guard = self
            .tool_overrides
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        guard.entry(name.to_string()).or_insert(handler);
    }

    pub fn register_tool_spec_provider(
        &self,
        provider: Arc<dyn crate::tool_override::ToolSpecProvider>,
    ) {
        if let Ok(mut guard) = self.tool_spec_providers.lock() {
            guard.push(provider);
        }
    }

    pub fn register_prompt_section_provider(
        &self,
        provider: Arc<dyn crate::tool_override::PromptSectionProvider>,
    ) {
        if let Ok(mut guard) = self.prompt_section_providers.lock() {
            guard.push(provider);
        }
    }

    pub fn collect_plugin_prompt_sections(&self) -> Vec<String> {
        self.prompt_section_providers
            .lock()
            .ok()
            .map(|guard| guard.iter().flat_map(|p| p.prompt_sections()).collect())
            .unwrap_or_default()
    }

    // ===== 配置查询 =====

    pub fn llm_config(&self) -> Option<&crate::core_config::LlmConfig> {
        self.core_config.as_ref().map(|c| &c.llm)
    }

    pub fn chat_is_multimodal(&self) -> bool {
        self.models_config.chat_is_multimodal()
    }

    pub fn provider_label(&self) -> String {
        self.core_config
            .as_ref()
            .map(|c| c.llm.chat.base_url.clone())
            .unwrap_or_default()
    }

    // ===== 权限与工具执行 =====

    /// 对工具进行权限检查
    pub(crate) fn check_tool_permission(
        &self,
        tool_name: &str,
    ) -> crate::permission::PermissionDecision {
        self.permission_gate.check(tool_name)
    }

    /// 执行单个工具调用（本地工具或后台任务）
    ///
    /// 注意：权限检查已由调用方（core/mod.rs）在执行前完成，
    /// 此方法内部的权限检查改为仅 Denied 拦截，NeedsApproval 由调用方处理。
    ///
    /// 所有工具均由各插件经 tool_overrides 统一分发，此方法不再持有具体领域参数。
    pub(crate) fn start_tool_call(
        &self,
        call: &ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        use crate::permission::PermissionDecision;
        match self.permission_gate.check(&call.name) {
            PermissionDecision::Approved => {}
            PermissionDecision::Denied { reason } => {
                return Box::pin(async move {
                    ToolResult {
                        ok: false,
                        summary: format!("权限拒绝：{reason}"),
                        stdout: String::new(),
                        stderr: reason,
                        exit_code: 1,
                        execution: None,
                    }
                });
            }
            PermissionDecision::NeedsApproval { .. } => {
                // 审批已由调用方完成，此处放行
            }
        }

        if let Some(handler) = self
            .tool_overrides
            .lock()
            .ok()
            .and_then(|g| g.get(&call.name).cloned())
        {
            let handler_future = handler.handle(call, session, actor_id);
            let tool_name = call.name.clone();
            return Box::pin(async move {
                if let Some(result) = handler_future.await {
                    return result;
                }

                ToolResult {
                    ok: false,
                    summary: format!("未注册的工具：{tool_name}（请确认对应插件已启用）"),
                    stdout: String::new(),
                    stderr: format!("tool {tool_name} not handled by any plugin"),
                    exit_code: 1,
                    execution: None,
                }
            });
        }

        let tool_name = call.name.clone();
        Box::pin(async move {
            ToolResult {
                ok: false,
                summary: format!("未注册的工具：{tool_name}（请确认对应插件已启用）"),
                stdout: String::new(),
                stderr: format!("tool {tool_name} not handled by any plugin"),
                exit_code: 1,
                execution: None,
            }
        })
    }
}
