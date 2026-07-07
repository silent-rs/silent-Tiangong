//! Agent Team 插件：子 Agent 管理 + 文件锁工具（issue #200）。
//!
//! 迁自 `tiangong-core/src/agent_team/` 与 `tiangong-core/src/react/team_bridge.rs`，
//! 改为插件 `tool_specs` 注册。7 个团队工具经统一的 `tool_overrides` 分发：
//! - `create_agent` / `dismiss_agent`：子 Agent 生命周期
//! - `send_message` / `broadcast_message` / `notify_user`：消息路由
//! - `lock_file` / `unlock_file`：文件编辑锁
//!
//! 子 Agent 的 ReAct 调度由本插件承接（spawn-per-message），经 core 暴露的
//! `ReactEngine::execute_turn` 递归执行；`TeamContext` 由插件独占持有。子 Agent ↔
//! 主 Agent 的全部通信经 feedback 通道（流事件 / usage / 汇报注入）；子 Agent 间
//! 通信走 message bus（`TeamContext.registry` 收件箱）。**不扩展 Plugin trait**。

pub mod constants;
pub mod handler;
pub mod lifecycle;
pub mod plugin;
pub mod state;
pub mod team_bridge;

pub use plugin::AgentTeamPlugin;
pub use state::{
    AgentDescriptor, AgentMessage, AgentRegistry, AgentStatus, FileLock, FileLockManager,
    MessagePriority,
};

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 团队执行上下文（迁自 core 的 `TeamContext`）。
///
/// 持有 Agent 注册表（含 message bus 收件箱）、文件锁管理器、主 Agent 收件箱。
/// 由本插件独占持有（`Arc<Mutex<TeamContext>>`），子 Agent 经 clone 共享同一份。
///
/// 与原 core 实现的差异：移除 `active_agent_senders` / `dispatch_waker`（原用于
/// `team_bridge` 的内联调度循环）。迁入插件后采用 spawn-per-message：投递到 Idle
/// Agent 收件箱的消息由 handler 直接触发该 Agent 的 `execute_turn`（异步 spawn），
/// 不再需要 waker 唤醒；运行中的 Agent 收到的新消息在收件箱排队，等下一轮派发。
pub struct TeamContext {
    /// Agent 注册表（含 message bus 收件箱 + 独立 Session）
    pub registry: AgentRegistry,
    /// 文件锁管理器
    pub file_locks: FileLockManager,
    /// 发给主 Agent 的消息（子 Agent 汇报）
    pub main_inbox: Vec<AgentMessage>,
}

impl TeamContext {
    pub fn new() -> Self {
        Self {
            registry: AgentRegistry::new(),
            file_locks: FileLockManager::new(),
            main_inbox: Vec::new(),
        }
    }

    pub fn deliver_main_message(&mut self, message: AgentMessage) {
        self.main_inbox.push(message);
    }

    pub fn drain_main_inbox(&mut self) -> Vec<AgentMessage> {
        std::mem::take(&mut self.main_inbox)
    }

    /// 投递消息到目标 Agent 的收件箱。
    ///
    /// 迁入插件后简化为单纯的收件箱投递（原 core 的 `dispatch_agent_message` 会尝试
    /// 实时注入运行中的 Agent；现统一走收件箱，由 spawn-per-message 调度消费）。
    /// `media` 暂存于消息体之外，由调度时合并进子 Agent 的首条 user 消息。
    pub fn dispatch_agent_message(
        &mut self,
        agent_id: &str,
        message: AgentMessage,
        _media: Vec<tiangong_types::MediaAsset>,
    ) {
        self.registry.deliver_message(agent_id, message);
    }
}

impl Default for TeamContext {
    fn default() -> Self {
        Self::new()
    }
}

// ── 插件入口 ──

/// 构造插件实例。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(AgentTeamPlugin::new())
}

/// 返回默认插件集合（供入口层注册）。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
