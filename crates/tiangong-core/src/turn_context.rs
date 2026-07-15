//! 一轮对话的执行上下文。
//!
//! [`TurnContext`] 的生命周期严格限制为单个 turn：收到 Message 时由 `build_turn_context`
//! 构造,turn 结束后整体销毁。它持有 turn 执行所需的 client / 权限 / 工具 / 用量收集器。
//!
//! 与 `react/` 模块的关系:`TurnContext` 是被 react 层消费的能力集合,本身不属于
//! ReAct 执行流程。`react/engine.rs` 在 `impl TurnContext` 上定义 `execute_turn`
//! 等 ReAct 循环方法。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::agent_config::AgentConfig;
use crate::model::{SingleProviderClient, ToolCall, ToolSpec};
use crate::session::Session;
use crate::tool::ToolResult;
use crate::tool_override::ToolOverrideHandler;

/// 一轮对话的执行上下文（替代原 ReactEngine + RuntimeEngine）。
///
/// 生命周期严格限制为单个 turn：收到 Message 时构造,
/// turn 结束后整体销毁。不跨 turn 复用。
#[derive(Clone)]
pub struct TurnContext {
    /// 模型请求客户端
    pub client: SingleProviderClient,
    /// 上下文 token 上限
    pub context_limit: usize,
    /// Agent 配置（reasoning_effort 等）
    pub agent_config: AgentConfig,
    /// 会话信任模式（FullTrust 放行一切,否则需审批;审批在 turn 层统一完成）
    pub trust_mode: crate::permission::TrustMode,
    /// 观测器（审计日志写入,持有 storage_root）
    pub observer: crate::observe::Observer,
    /// 工具覆盖处理器（会话级共享 Arc）
    pub tool_overrides: Arc<Mutex<HashMap<String, Arc<dyn ToolOverrideHandler>>>>,
    /// Plugin 注册的 Prompt 段落提供者（会话级共享 Arc,供 rebuild_system_prompt 收集）
    pub prompt_section_providers:
        Arc<Mutex<Vec<Arc<dyn crate::tool_override::PromptSectionProvider>>>>,
    /// Turn-scoped 插件 usage 收集器
    pub turn_usage_sink: Arc<crate::core::plugin::TurnUsageSink>,
    // ===== turn 级配置 =====
    /// 当前执行单元可用的工具集
    pub tools: Vec<ToolSpec>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: SingleProviderClient,
        context_limit: usize,
        agent_config: AgentConfig,
        trust_mode: crate::permission::TrustMode,
        observer: crate::observe::Observer,
        tools: Vec<ToolSpec>,
        max_tool_rounds: usize,
        max_outer_iterations: u32,
        turn_usage_sink: Arc<crate::core::plugin::TurnUsageSink>,
    ) -> Self {
        Self {
            client,
            context_limit,
            agent_config,
            trust_mode,
            observer,
            tool_overrides: Arc::new(Mutex::new(HashMap::new())),
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

    /// 注入会话级共享的插件注册表（跨 turn 复用,避免每 turn 重复注册）。
    pub fn with_shared_plugin_state(
        mut self,
        tool_overrides: Arc<Mutex<HashMap<String, Arc<dyn ToolOverrideHandler>>>>,
        prompt_section_providers: Arc<
            Mutex<Vec<Arc<dyn crate::tool_override::PromptSectionProvider>>>,
        >,
    ) -> Self {
        self.tool_overrides = tool_overrides;
        self.prompt_section_providers = prompt_section_providers;
        self
    }

    // ===== 能力 accessor =====

    pub fn client(&self) -> &SingleProviderClient {
        &self.client
    }

    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent_config
    }

    pub fn turn_usage_sink(&self) -> &Arc<crate::core::plugin::TurnUsageSink> {
        &self.turn_usage_sink
    }

    // ===== 插件注册 =====

    pub fn register_tool_override(&self, name: &str, handler: Arc<dyn ToolOverrideHandler>) {
        let mut guard = self
            .tool_overrides
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        guard.entry(name.to_string()).or_insert(handler);
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

    // ===== 工具执行 =====

    /// 执行单个工具调用。
    ///
    /// 权限审批在 turn 层统一完成（engine.rs 的工具执行循环）;
    /// 到达此方法时审批已通过,handler 直接执行。
    pub(crate) fn start_tool_call(
        &self,
        call: &ToolCall,
        session: &mut Session,
        actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
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
