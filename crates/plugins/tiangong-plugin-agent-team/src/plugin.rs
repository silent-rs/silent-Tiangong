//! Agent Team 插件结构体与生命周期实现。
//!
//! [`AgentTeamPlugin`] 经以下方式获取运行时上下文：
//! - **构造**：`team: Arc<Mutex<TeamContext>>` 进程级共享，跨子引擎 clone。
//! - [`Plugin::register`]：捕获 `RuntimeEngine` clone，供子 Agent `execute_turn`
//!   构造子 ReactEngine 用。
//! - [`Plugin::set_feedback_tx`]：注入状态反馈通道（转发流事件、上报 usage、注入汇报）。
//! - [`Plugin::on_turn_started`]：路由用户 @提及到目标 Agent 并 spawn。
//! - [`Plugin::on_engine_rebuilt`] / [`Plugin::on_session_ready`]：从会话历史恢复 Agent。
//!
//! ## 实例模型
//!
//! `AgentTeamPlugin` 实例是 **per-Core** 的（每次 engine 创建现场构造）。`team`
//! 持有的 `TeamContext` 也随之 per-Core——同一时刻一个 Core 一个 session 一个团队。
//! `runtime_engine` 在 `register` 时 clone 捕获（与父引擎共享同一 `tool_overrides`）。
//!
//! ## 文件锁
//!
//! 文件锁（`lock_file` / `unlock_file`）仅由本插件内部的 `FileLockManager` 管理，
//! 不拦截外部 write 工具——多 Agent 编辑冲突由 Agent 自觉遵守 lock_file 协议。

use std::sync::{Arc, Mutex, RwLock};

use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::model::ToolSpec;
use tiangong_core::prompt::SystemPromptConfig;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::session::Session;
use tiangong_core::tool_override::PromptSectionProvider;

use crate::lifecycle::restore_agents_from_session_history;
use crate::team_bridge::PromptConfig;
use crate::TeamContext;

/// 当前调用方身份（默认 "main"，子 Agent turn 内由其引擎 agent_id 决定）。
const MAIN_AGENT_ID: &str = "main";

/// Agent Team 插件。
pub struct AgentTeamPlugin {
    /// 团队上下文（进程级共享，跨子引擎 clone）。
    pub team: Arc<Mutex<TeamContext>>,
    /// 状态反馈通道（转发子 Agent 流事件、上报 usage、注入汇报）。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
    /// 父 RuntimeEngine clone（register 时捕获，供子 Agent 构造子 ReactEngine）。
    runtime_engine: RwLock<Option<RuntimeEngine>>,
    /// 父工具快照（register 时捕获，供子 Agent 过滤可用工具）。
    parent_tools: RwLock<Vec<ToolSpec>>,
    /// 当前会话 id（on_session_ready 设置，供 child session 持久化）。
    session_id: RwLock<Option<String>>,
    /// PromptConfig（on_engine_rebuilt 构建，供子 Agent system prompt）。
    prompt_config: RwLock<Option<Arc<PromptConfig>>>,
}

impl AgentTeamPlugin {
    pub fn new() -> Self {
        Self {
            team: Arc::new(Mutex::new(TeamContext::new())),
            feedback_tx: RwLock::new(None),
            runtime_engine: RwLock::new(None),
            parent_tools: RwLock::new(Vec::new()),
            session_id: RwLock::new(None),
            prompt_config: RwLock::new(None),
        }
    }

    /// 读取反馈通道的 clone（供 handler 转发流事件用）。
    pub(crate) fn feedback_tx(&self) -> Option<PluginFeedbackTx> {
        self.feedback_tx.read().ok()?.as_ref().cloned()
    }

    /// 当前调用方身份。
    ///
    /// 工具经 `tool_overrides` 分发时，handler 收到的 `&Session` 是当前调用方的
    /// session：子 Agent 调用时是其 child_session（`active_agent_id = Some(agent_id)`），
    /// 主 Agent 调用时是主 session（`active_agent_id = None`）。据此识别身份，
    /// 无需扩展 core trait。
    pub(crate) fn current_agent_id(&self, session: &Session) -> String {
        session
            .active_agent_id
            .clone()
            .unwrap_or_else(|| MAIN_AGENT_ID.to_string())
    }

    /// 父工具快照 clone（供 execute_team_tool 解析子 Agent 可用工具 + 子引擎构造）。
    pub(crate) fn parent_tools_snapshot(&self) -> Vec<ToolSpec> {
        self.parent_tools
            .read()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// 父 RuntimeEngine clone 快照（供 handler 构造子 ReactEngine）。
    pub(crate) fn runtime_engine_snapshot(&self) -> Option<RuntimeEngine> {
        self.runtime_engine.read().ok()?.as_ref().cloned()
    }

    /// PromptConfig 快照（供子 Agent system prompt 构建）。
    pub(crate) fn prompt_config_snapshot(&self) -> Option<Arc<PromptConfig>> {
        self.prompt_config.read().ok()?.clone()
    }
}

impl Default for AgentTeamPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for AgentTeamPlugin {
    fn id(&self) -> &str {
        "agent_team"
    }

    fn register(&self, engine: &RuntimeEngine) {
        // 捕获父 RuntimeEngine clone（子 Agent 经此继承 tool_overrides）。
        if let Ok(mut guard) = self.runtime_engine.write() {
            *guard = Some(engine.clone());
        }
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
    }

    fn on_session_ready(&self, session: &mut Session) {
        if let Ok(mut guard) = self.session_id.write() {
            *guard = Some(session.id.clone());
        }
        // 从 engine 的 tool_spec_providers 收集全部工具规格，供子 Agent 过滤可用工具。
        self.refresh_parent_tools();
        // 构建 PromptConfig（需要 models_config + agent_config）。
        self.rebuild_prompt_config(session);
        // 从会话历史恢复 Agent（崩溃恢复）。
        let tools = self.parent_tools_snapshot();
        if let Ok(mut team) = self.team.lock() {
            restore_agents_from_session_history(&mut team, session, &tools);
        }
    }

    fn on_engine_rebuilt(&self, session: &mut Session) {
        // engine 重建后工具列表可能变化（插件增减），重新收集。
        self.refresh_parent_tools();
        self.rebuild_prompt_config(session);
    }

    // on_turn_started 不做 @路由：用户输入中的 @提及由主 Agent 自行决定调用
    // send_message 投递（与 LLM 主动发消息完全同构），保持所有子 Agent 交互统一
    // 经工具调用路径。

    fn tool_permission_overrides(
        &self,
    ) -> std::collections::BTreeMap<String, tiangong_core::permission::PermissionLevel> {
        // 团队工具均为无副作用的管理操作（创建/解散 Agent、消息路由、通知、文件锁），
        // 声明为 Safe 避免 core 默认 classify_tool 把未知工具名归为 Critical（需要审批）。
        let mut overrides = std::collections::BTreeMap::new();
        for name in [
            "create_agent",
            "dismiss_agent",
            "send_message",
            "broadcast_message",
            "notify_user",
            "lock_file",
            "unlock_file",
        ] {
            overrides.insert(
                name.to_string(),
                tiangong_core::permission::PermissionLevel::Safe,
            );
        }
        overrides
    }
}

impl AgentTeamPlugin {
    /// 构建/刷新 PromptConfig（从父 RuntimeEngine 的配置 + 当前 session）。
    fn rebuild_prompt_config(&self, session: &Session) {
        let Some(engine) = self.runtime_engine.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        let base = SystemPromptConfig::from_configs(engine.models_config(), engine.agent_config());
        let prompt_config = Arc::new(PromptConfig {
            session_id: session.id.clone(),
            base: Arc::new(base),
        });
        if let Ok(mut guard) = self.prompt_config.write() {
            *guard = Some(prompt_config);
        }
    }

    /// 从父 RuntimeEngine 的 tool_spec_providers 收集全部工具规格。
    ///
    /// 子 Agent 创建时可选指定 tools 列表（默认继承全部），run_agent_turn 用此快照
    /// 过滤出子 Agent 可用工具。engine 重建后（插件增减导致工具变化）需重新收集。
    fn refresh_parent_tools(&self) {
        let Some(engine) = self.runtime_engine.read().ok().and_then(|g| g.clone()) else {
            return;
        };
        let tools: Vec<ToolSpec> = engine
            .tool_spec_providers()
            .iter()
            .flat_map(|provider| provider.tool_specs())
            .collect();
        if let Ok(mut guard) = self.parent_tools.write() {
            *guard = tools;
        }
    }
}

/// 注入团队工具使用指引（迁自 core `prompt/sections.rs::build_agent_team_section`）。
impl PromptSectionProvider for AgentTeamPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        vec![build_agent_team_section()]
    }
}

fn build_agent_team_section() -> String {
    "团队协作能力（可选使用）：
当任务复杂需要分工时，你可以创建团队来协作完成。以下是可用的团队工具：

- create_agent(role, label, system_prompt, tools)：创建一个 Sub Agent
  - role：角色标识（如 pm、dev、test），用于消息路由
  - label：显示名称（如「项目经理」「开发者」）
  - system_prompt：Agent 的专属指令
  - tools：可选，指定可用工具列表（默认继承你的工具集，不含 create_agent/dismiss_agent）
  - Agent 持续存在直到被解散
  - 最多同时 8 个 Agent

- send_message(to, content)：向指定角色的 Agent 发送消息，Agent 会自动执行任务
- broadcast_message(content, exclude)：向所有 Agent 广播消息
- notify_user(content, level)：向用户推送通知（info/warning/error）

- lock_file(path)：获取文件编辑锁（编辑前必先加锁，防止多 Agent 冲突）
- unlock_file(path)：释放文件编辑锁
- dismiss_agent(role)：解散指定 Agent，释放其持有的所有资源

使用要点：
1. 复杂任务先拆解，为每个子任务创建专职 Agent（明确的 system_prompt）。
2. 通过 send_message 分配任务；send_message 会等待目标 Agent 执行完成并把其汇报作为结果返回给你。
3. 用户输入中的 @提及（如「@dev 检查这个改动」）应转成 send_message(to=dev, content=检查这个改动)。
4. 多 Agent 编辑同一文件时，编辑前必须 lock_file，编辑后 unlock_file。
"
    .to_string()
}

// 静默未使用 import 警告（MediaAsset/StreamEvent 在其他模块使用）。
