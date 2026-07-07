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
    /// 运行中子 Agent 的命令通道句柄（供 cancel 路由）。
    /// agent_id → cmd_tx；Agent 开始执行时注册，结束/取消时注销。
    active_agent_senders: std::collections::HashMap<
        String,
        tokio::sync::mpsc::UnboundedSender<tiangong_core::core::command::Command>,
    >,
}

impl TeamContext {
    pub fn new() -> Self {
        Self {
            registry: AgentRegistry::new(),
            file_locks: FileLockManager::new(),
            main_inbox: Vec::new(),
            active_agent_senders: std::collections::HashMap::new(),
        }
    }

    pub fn deliver_main_message(&mut self, message: AgentMessage) {
        self.main_inbox.push(message);
    }

    pub fn drain_main_inbox(&mut self) -> Vec<AgentMessage> {
        std::mem::take(&mut self.main_inbox)
    }

    /// 注册运行中子 Agent 的命令通道（执行开始时调用）。
    pub fn register_active_agent(
        &mut self,
        agent_id: String,
        tx: tokio::sync::mpsc::UnboundedSender<tiangong_core::core::command::Command>,
    ) {
        self.active_agent_senders.insert(agent_id, tx);
    }

    /// 注销子 Agent 的命令通道（执行结束/取消时调用）。
    pub fn unregister_active_agent(&mut self, agent_id: &str) {
        self.active_agent_senders.remove(agent_id);
    }

    /// 向运行中的子 Agent 发送命令（如 Cancel）。返回是否投递成功。
    pub fn send_to_active_agent(
        &self,
        agent_id: &str,
        cmd: tiangong_core::core::command::Command,
    ) -> bool {
        self.active_agent_senders
            .get(agent_id)
            .map(|tx| tx.send(cmd).is_ok())
            .unwrap_or(false)
    }

    /// 按 role 查找运行中子 Agent 的 agent_id（供 cancel_agent(role) 路由）。
    pub fn active_agent_id_by_role(&self, role: &str) -> Option<String> {
        let role = role.trim().trim_start_matches('@');
        let agent = self.registry.find_by_role(role)?;
        if self.active_agent_senders.contains_key(&agent.agent_id) {
            Some(agent.agent_id.clone())
        } else {
            None
        }
    }

    /// 当前运行中子 Agent 的 cmd_tx 句柄表（只读视图，供调度判定 Agent 是否运行中）。
    pub fn active_agent_senders(
        &self,
    ) -> &std::collections::HashMap<
        String,
        tokio::sync::mpsc::UnboundedSender<tiangong_core::core::command::Command>,
    > {
        &self.active_agent_senders
    }

    /// 投递消息到目标 Agent。
    ///
    /// 若目标 Agent 正在运行（已注册 cmd_tx），经 cmd_tx 实时注入其当前 execute_turn
    /// 循环（`Command::Message`）；否则投到收件箱排队，等下一轮派发时消费。
    /// `media` 随消息一起经 Command::Message 投递（实时注入时）或并入收件箱消息。
    pub fn dispatch_agent_message(
        &mut self,
        agent_id: &str,
        message: AgentMessage,
        media: Vec<tiangong_types::MediaAsset>,
    ) {
        let content = format!(
            "[from:{} at {}]\n{}",
            message.from, message.created_at, message.content
        );
        let dispatched = if let Some(tx) = self.active_agent_senders.get(agent_id) {
            tx.send(tiangong_core::core::command::Command::Message {
                content,
                message_id: Some(message.id.clone()),
                media,
            })
            .is_ok()
        } else {
            false
        };
        if !dispatched {
            self.registry.deliver_message(agent_id, message);
        }
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
