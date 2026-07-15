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
    /// 本轮会话（turn 期间独占,turn 结束时取回落盘）
    pub session: Session,
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
        session: Session,
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
            session,
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

// ===== TurnContextBuilder =====

use std::path::Path;

use crate::core::plugin::Plugin;
use crate::core::plugin::PluginFeedbackTx;
use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::OnRetryCallback;
use crate::observe::Observer;
use crate::permission::TrustMode;
use tiangong_types::StreamEvent;

/// TurnContext 构造器。
///
/// 在 deliver(Message) 中使用:从 CoreConfig 构建 client/权限,
/// 注册插件,注入 session,产出完整 TurnContext。
pub struct TurnContextBuilder {
    config: CoreConfig,
    trust_mode: TrustMode,
    storage_root: std::path::PathBuf,
    session: Option<Session>,
    plugins: Vec<Arc<dyn Plugin>>,
    stream_tx: Option<std::sync::mpsc::Sender<StreamEvent>>,
    cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::core::command::Command>>,
}

impl TurnContextBuilder {
    pub fn new(
        config: CoreConfig,
        trust_mode: TrustMode,
        storage_root: std::path::PathBuf,
    ) -> Self {
        Self {
            config,
            trust_mode,
            storage_root,
            session: None,
            plugins: Vec::new(),
            stream_tx: None,
            cmd_tx: None,
        }
    }

    /// 注入 session(用户消息已注入并落盘)。
    pub fn session(mut self, session: Session) -> Self {
        self.session = Some(session);
        self
    }

    /// 进程内插件。
    pub fn plugins(mut self, plugins: Vec<Arc<dyn Plugin>>) -> Self {
        self.plugins = plugins;
        self
    }

    /// 内部 stream 通道(forwarder 用)。
    pub fn stream_tx(mut self, tx: std::sync::mpsc::Sender<StreamEvent>) -> Self {
        self.stream_tx = Some(tx);
        self
    }

    /// 命令通道(给插件 feedback)。
    pub fn cmd_tx(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::core::command::Command>,
    ) -> Self {
        self.cmd_tx = Some(tx);
        self
    }

    /// 构建 TurnContext。
    ///
    /// 完成插件注册(on_config_updated / set_workspace / set_trust_mode / set_feedback_tx /
    /// tool_specs / tool_overrides / prompt_sections / exec_env / on_session_ready)。
    pub fn build(self) -> Result<TurnContext, String> {
        let session = self.session.ok_or("session 未设置")?;
        let stream_tx = self.stream_tx.ok_or("stream_tx 未设置")?;
        let cmd_tx = self.cmd_tx.ok_or("cmd_tx 未设置")?;

        // 构建 client + observer
        let agent_config = crate::agent_config::AgentConfig {
            trust_mode: self.config.trust_mode,
            default_trust_mode: self.config.default_trust_mode,
            custom_system_prompt: self.config.custom_system_prompt.clone(),
            reasoning_effort: self.config.reasoning_effort.clone(),
        };

        let retry_tx = stream_tx.clone();
        let on_retry: OnRetryCallback =
            Arc::new(move |attempt, max_attempts, _delay_ms, error_text| {
                let _ = retry_tx.send(StreamEvent::Retry {
                    message: error_text.to_string(),
                    attempt,
                    max_attempts,
                });
            });

        let context_limit = self.config.context_limit;
        let client =
            SingleProviderClient::new(self.config.llm.chat.clone()).with_on_retry(on_retry);

        let observer = Observer::new(self.storage_root.clone());
        let usage_sink = Arc::new(crate::core::plugin::TurnUsageSink::new());

        let mut ctx = TurnContext::new(
            session,
            client,
            context_limit,
            agent_config,
            self.trust_mode,
            observer,
            Vec::new(),
            crate::MAX_TOOL_ROUNDS,
            crate::MAX_OUTER_ITERATIONS,
            usage_sink,
        );

        // 插件注册
        let workspace = std::path::Path::new(&ctx.session.cwd);
        let workspace = workspace.is_dir().then_some(workspace);

        for plugin in &self.plugins {
            plugin.on_config_updated(&self.config);
        }

        let mut plugin_specs: Vec<ToolSpec> = Vec::new();
        let mut seen_tool_names = std::collections::HashSet::new();

        for plugin in &self.plugins {
            plugin.set_workspace(workspace);
            plugin.set_trust_mode(self.trust_mode);
            plugin.set_feedback_tx(PluginFeedbackTx::new(
                cmd_tx.clone(),
                ctx.turn_usage_sink().clone(),
            ));

            let specs = plugin.tool_specs();
            for spec in &specs {
                ctx.register_tool_override(&spec.name, plugin.clone());
            }
            ctx.register_prompt_section_provider(plugin.clone());

            for spec in specs {
                if seen_tool_names.insert(spec.name.clone()) {
                    plugin_specs.push(spec);
                }
            }
        }

        // exec_env
        let mut exec_env = std::collections::BTreeMap::new();
        for plugin in &self.plugins {
            for (key, value) in plugin.exec_env() {
                exec_env.insert(key, value);
            }
        }
        for plugin in &self.plugins {
            plugin.set_exec_env(exec_env.clone());
        }

        // tools
        let injection_spec = crate::core::plugin::injection_tool_spec();
        ctx.tools.push(injection_spec);
        ctx.tools.extend(plugin_specs);

        // on_session_ready(每次 turn task 都触发——turn task 模型下不存在跨 turn 状态)
        for plugin in &self.plugins {
            plugin.on_session_ready(&mut ctx.session);
        }

        Ok(ctx)
    }
}
