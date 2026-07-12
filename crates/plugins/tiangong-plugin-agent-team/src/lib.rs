//! Agent Team 插件：子 Agent 管理 + 文件锁工具（issue #200）。
//!
//! 迁自 `tiangong-core/src/agent_team/` 与 `tiangong-core/src/react/team_bridge.rs`，
//! 改为插件 `tool_specs` 注册。7 个团队工具经统一的 `tool_overrides` 分发：
//! - `create_agent` / `dismiss_agent`：子 Agent 生命周期
//! - `send_message` / `broadcast_message` / `notify_user`：消息路由
//! - `lock_file` / `unlock_file`：文件编辑锁
//!
//! 子 Agent 的 ReAct 调度由本插件承接（每个 Agent 串行消费收件箱），经 core 暴露的
//! `ReactEngine::execute_turn` 递归执行；`TeamContext` 由插件独占持有。子 Agent ↔
//! 主 Agent 的全部通信经 feedback 通道（流事件 / usage / 汇报注入）；子 Agent 间
//! 通信走 message bus（`TeamContext.registry` 收件箱）。消息规划、运行时控制和关闭
//! 均经通用 Plugin 生命周期扩展点接入 Core。

mod cancellation;
mod command_safety;
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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tiangong_core::core::command::Command;
use tiangong_core::core::Plugin;

use crate::state::message_bus::AgentInboxEntry;

/// 团队执行上下文（迁自 core 的 `TeamContext`）。
///
/// 持有 Agent 注册表（含 message bus 收件箱）、文件锁管理器、主 Agent 收件箱。
/// 由本插件独占持有（`Arc<Mutex<TeamContext>>`），子 Agent 经 clone 共享同一份。
///
/// 运行中句柄和调度集合都由插件持有，确保同一个 Agent 串行消费收件箱，同时允许
/// 不同 Agent 在全局并发上限内并行执行。
pub struct TeamContext {
    /// Agent 注册表（含 message bus 收件箱 + 独立 Session）
    pub registry: AgentRegistry,
    /// 文件锁管理器
    pub file_locks: FileLockManager,
    /// 发给主 Agent 的消息（子 Agent 汇报）
    pub main_inbox: Vec<AgentMessage>,
    /// main 消息 ID → 产生该消息的稳定工作 ID，仅绑定当前活跃执行尝试。
    main_message_attempts: HashMap<String, String>,
    /// 运行中子 Agent 的命令通道句柄（供 cancel 路由）。
    /// agent_id → cmd_tx；Agent 开始执行时注册，结束/取消时注销。
    active_agents: HashMap<String, ActiveAgentHandle>,
    /// 已提交后台任务但尚未进入执行体的 Agent。
    scheduled_agents: HashSet<String>,
    /// 正由后台任务串行消费收件箱的 Agent。
    in_flight_agents: HashSet<String>,
}

/// 运行中子 Agent 的控制句柄。
#[derive(Clone)]
pub struct ActiveAgentHandle {
    pub command_tx: tokio::sync::mpsc::UnboundedSender<Command>,
    pub cancel_flag: Arc<AtomicBool>,
    pub shutdown_flag: Arc<AtomicBool>,
    pub pending_delivery_id: Option<String>,
}

impl TeamContext {
    pub fn new() -> Self {
        Self {
            registry: AgentRegistry::new(),
            file_locks: FileLockManager::new(),
            main_inbox: Vec::new(),
            main_message_attempts: HashMap::new(),
            active_agents: HashMap::new(),
            scheduled_agents: HashSet::new(),
            in_flight_agents: HashSet::new(),
        }
    }

    pub fn deliver_main_message(&mut self, message: AgentMessage) {
        self.main_message_attempts.remove(&message.id);
        if let Some(work_id) = self
            .active_agents
            .get(&message.from)
            .and_then(|handle| handle.pending_delivery_id.clone())
        {
            self.main_message_attempts
                .insert(message.id.clone(), work_id);
        }
        if let Some(existing) = self
            .main_inbox
            .iter()
            .position(|existing| existing.id == message.id)
        {
            self.main_inbox[existing] = message;
        } else {
            self.main_inbox.push(message);
        }
    }

    pub fn drain_main_inbox(&mut self) -> Vec<AgentMessage> {
        self.main_message_attempts.clear();
        std::mem::take(&mut self.main_inbox)
    }

    /// 查询指定稳定工作在当前执行尝试内产生的主收件箱消息。
    pub(crate) fn main_messages_for_work(&self, work_id: &str) -> Vec<AgentMessage> {
        self.main_inbox
            .iter()
            .filter(|message| {
                self.main_message_attempts
                    .get(&message.id)
                    .is_some_and(|bound_work_id| bound_work_id == work_id)
            })
            .cloned()
            .collect()
    }

    /// 精确删除指定稳定工作在当前执行尝试内产生的主收件箱消息。
    pub(crate) fn remove_main_messages_for_work(&mut self, work_id: &str) -> usize {
        let message_ids = self
            .main_message_attempts
            .iter()
            .filter(|(_, bound_work_id)| bound_work_id.as_str() == work_id)
            .map(|(message_id, _)| message_id.clone())
            .collect::<HashSet<_>>();
        if message_ids.is_empty() {
            return 0;
        }
        let before = self.main_inbox.len();
        self.main_inbox
            .retain(|message| !message_ids.contains(&message.id));
        self.main_message_attempts
            .retain(|message_id, _| !message_ids.contains(message_id));
        before.saturating_sub(self.main_inbox.len())
    }

    /// 按消息 ID 删除主收件箱内容，并同步清除执行尝试绑定。
    pub(crate) fn remove_main_messages_by_ids(&mut self, message_ids: &[String]) -> usize {
        if message_ids.is_empty() {
            return 0;
        }
        let message_ids = message_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let before = self.main_inbox.len();
        self.main_inbox
            .retain(|message| !message_ids.contains(message.id.as_str()));
        self.main_message_attempts
            .retain(|message_id, _| !message_ids.contains(message_id.as_str()));
        before.saturating_sub(self.main_inbox.len())
    }

    /// 注册运行中子 Agent 的命令通道（执行开始时调用）。
    pub fn register_active_agent(
        &mut self,
        agent_id: String,
        command_tx: tokio::sync::mpsc::UnboundedSender<Command>,
        cancel_flag: Arc<AtomicBool>,
        shutdown_flag: Arc<AtomicBool>,
        pending_delivery_id: Option<String>,
    ) {
        self.active_agents.insert(
            agent_id,
            ActiveAgentHandle {
                command_tx,
                cancel_flag,
                shutdown_flag,
                pending_delivery_id,
            },
        );
    }

    /// 注销子 Agent 的命令通道（执行结束/取消时调用）。
    pub fn unregister_active_agent(&mut self, agent_id: &str) {
        self.active_agents.remove(agent_id);
    }

    /// 向运行中的子 Agent 发送命令（如 Cancel）。返回是否投递成功。
    pub fn send_to_active_agent(
        &self,
        agent_id: &str,
        cmd: tiangong_core::core::command::Command,
    ) -> bool {
        self.active_agents
            .get(agent_id)
            .map(|handle| handle.command_tx.send(cmd).is_ok())
            .unwrap_or(false)
    }

    /// 按 role 查找运行中子 Agent 的 agent_id（供 cancel_agent(role) 路由）。
    pub fn active_agent_id_by_role(&self, role: &str) -> Option<String> {
        let role = role.trim().trim_start_matches('@');
        let agent = self.registry.find_by_role(role)?;
        if self.active_agents.contains_key(&agent.agent_id) {
            Some(agent.agent_id.clone())
        } else {
            None
        }
    }

    /// 当前运行中子 Agent 的 cmd_tx 句柄表（只读视图，供调度判定 Agent 是否运行中）。
    pub fn is_agent_active(&self, agent_id: &str) -> bool {
        self.active_agents.contains_key(agent_id)
    }

    pub fn active_agent_handle(&self, agent_id: &str) -> Option<ActiveAgentHandle> {
        self.active_agents.get(agent_id).cloned()
    }

    pub fn active_agent_handles(&self) -> Vec<ActiveAgentHandle> {
        self.active_agents.values().cloned().collect()
    }

    pub fn active_agent_ids(&self) -> Vec<String> {
        self.active_agents.keys().cloned().collect()
    }

    /// 原子占用一个 Agent 的后台调度槽，防止重复 spawn。
    pub fn try_mark_scheduled(&mut self, agent_id: &str) -> bool {
        if self.active_agents.contains_key(agent_id)
            || self.scheduled_agents.contains(agent_id)
            || self.in_flight_agents.contains(agent_id)
            || !self.registry.has_pending_inbox_for(agent_id)
        {
            return false;
        }
        self.scheduled_agents.insert(agent_id.to_string())
    }

    pub fn begin_scheduled(&mut self, agent_id: &str) {
        self.scheduled_agents.remove(agent_id);
        self.in_flight_agents.insert(agent_id.to_string());
    }

    pub fn finish_in_flight(&mut self, agent_id: &str) {
        self.scheduled_agents.remove(agent_id);
        self.in_flight_agents.remove(agent_id);
    }

    /// 在后台消费者准备退出时，原子判断是否还有待处理消息。
    ///
    /// 返回 `true` 表示应保持 `in_flight` 并继续消费；返回 `false` 时已在同一
    /// 临界区清除调度状态。这样新投递要么被当前消费者看到，要么能成功创建
    /// 下一位消费者，不会落入两次锁之间的空窗。
    pub fn finish_in_flight_if_idle(&mut self, agent_id: &str) -> bool {
        if self.registry.has_pending_inbox_for(agent_id) {
            true
        } else {
            self.finish_in_flight(agent_id);
            false
        }
    }

    pub fn is_in_flight(&self, agent_id: &str) -> bool {
        self.in_flight_agents.contains(agent_id)
    }

    /// 是否已有运行中、已提交或正在消费收件箱的执行波次。
    pub fn has_execution_work(&self) -> bool {
        !self.active_agents.is_empty()
            || !self.scheduled_agents.is_empty()
            || !self.in_flight_agents.is_empty()
    }

    /// 投递消息到目标 Agent。
    ///
    /// 若目标 Agent 正在运行（已注册 cmd_tx），经 cmd_tx 实时注入其当前 execute_turn
    /// 循环（`Command::Message`）；否则投到收件箱排队，等下一轮派发时消费。
    /// 完整内容块随 entry 一起经新版 `Command::Message` 投递；持久直达消息仍进入
    /// 收件箱，确保每个 delivery 独立执行并在落盘后确认。
    pub fn dispatch_agent_message(&mut self, agent_id: &str, entry: AgentInboxEntry) {
        let message_id = entry
            .session_message_id
            .clone()
            .unwrap_or_else(|| entry.message.id.clone());
        let prepared = crate::lifecycle::prepared_agent_message_for_prompt(
            &entry.message,
            entry.additional_content.clone(),
        );
        let dispatched = if entry.session_message_id.is_none() {
            self.active_agents
                .get(agent_id)
                .map(|handle| {
                    handle
                        .command_tx
                        .send(Command::Message {
                            prepared,
                            message_id: Some(message_id),
                            trust_mode_override: None,
                            persistence_ack: None,
                        })
                        .is_ok()
                })
                .unwrap_or(false)
        } else {
            false
        };
        if !dispatched {
            self.registry.deliver_inbox_entry(agent_id, entry);
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
pub fn build_plugin(storage_root: PathBuf) -> Arc<dyn Plugin> {
    Arc::new(AgentTeamPlugin::new(storage_root))
}

/// 把宿主的“取消 Agent”操作适配为 Core 的通用插件控制输入。
pub fn cancel_agent_input(role: impl Into<String>) -> tiangong_core::agent_input::AgentInputKind {
    tiangong_core::agent_input::AgentInputKind::plugin_control(
        constants::PLUGIN_ID,
        constants::CONTROL_CANCEL_AGENT,
        serde_json::json!({ "role": role.into() }),
    )
}

/// 返回默认插件集合（供入口层注册）。
pub fn default_plugins(storage_root: PathBuf) -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin(storage_root)]
}

#[cfg(test)]
mod tests {
    use tiangong_types::now_text;

    use super::*;
    use crate::state::message_bus::MessagePriority;

    const AGENT_ID: &str = "agent-dev";

    fn team_with_agent() -> TeamContext {
        let mut team = TeamContext::new();
        team.registry.register(crate::AgentDescriptor {
            agent_id: AGENT_ID.to_string(),
            role: "dev".to_string(),
            label: "Developer".to_string(),
            system_prompt: "Implement changes".to_string(),
            tools: Vec::new(),
            status: crate::AgentStatus::Idle,
        });
        team
    }

    fn deliver_message(team: &mut TeamContext, id: &str) {
        team.registry.deliver_message(
            AGENT_ID,
            AgentMessage {
                id: id.to_string(),
                from: "main".to_string(),
                to: AGENT_ID.to_string(),
                content: id.to_string(),
                priority: MessagePriority::Normal,
                created_at: now_text(),
            },
        );
    }

    #[test]
    fn finalization_keeps_consumer_when_message_is_already_pending() {
        let mut team = team_with_agent();
        deliver_message(&mut team, "message-1");
        assert!(team.try_mark_scheduled(AGENT_ID));
        team.begin_scheduled(AGENT_ID);
        team.registry
            .take_next_inbox_entry(AGENT_ID)
            .expect("first message should be claimed");
        deliver_message(&mut team, "message-2");

        assert!(team.finish_in_flight_if_idle(AGENT_ID));
        assert!(team.is_in_flight(AGENT_ID));
    }

    #[test]
    fn delivery_after_atomic_idle_transition_can_schedule_new_consumer() {
        let mut team = team_with_agent();
        deliver_message(&mut team, "message-1");
        assert!(team.try_mark_scheduled(AGENT_ID));
        team.begin_scheduled(AGENT_ID);
        team.registry
            .take_next_inbox_entry(AGENT_ID)
            .expect("first message should be claimed");

        assert!(!team.finish_in_flight_if_idle(AGENT_ID));
        assert!(!team.is_in_flight(AGENT_ID));

        deliver_message(&mut team, "message-2");
        assert!(team.try_mark_scheduled(AGENT_ID));
    }

    #[test]
    fn main_inbox_binds_messages_to_attempt_and_cleans_mappings_precisely() {
        let mut team = team_with_agent();
        let (command_tx, _command_rx) = tokio::sync::mpsc::unbounded_channel();
        team.register_active_agent(
            AGENT_ID.to_string(),
            command_tx.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Some("work-1".to_string()),
        );

        let main_message = |id: &str, content: &str| AgentMessage {
            id: id.to_string(),
            from: AGENT_ID.to_string(),
            to: "main".to_string(),
            content: content.to_string(),
            priority: MessagePriority::Normal,
            created_at: now_text(),
        };
        team.deliver_main_message(main_message("main-1", "first"));
        team.deliver_main_message(main_message("main-1", "retried"));
        assert_eq!(team.main_inbox.len(), 1, "相同稳定消息 ID 应替换而非重复");
        assert_eq!(team.main_messages_for_work("work-1")[0].content, "retried");

        // receipt 恢复消息可以直接进入 main_inbox，不绑定当前执行尝试。
        team.main_inbox
            .push(main_message("receipt-restored", "restored"));
        team.register_active_agent(
            AGENT_ID.to_string(),
            command_tx,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Some("work-2".to_string()),
        );
        team.deliver_main_message(main_message("main-2", "second work"));

        assert_eq!(team.remove_main_messages_for_work("work-1"), 1);
        assert!(team.main_messages_for_work("work-1").is_empty());
        assert_eq!(team.main_inbox.len(), 2);
        assert!(team
            .main_inbox
            .iter()
            .any(|message| message.id == "receipt-restored"));

        assert_eq!(team.remove_main_messages_by_ids(&["main-2".to_string()]), 1);
        assert!(team.main_messages_for_work("work-2").is_empty());
        assert_eq!(team.main_inbox.len(), 1);
        assert_eq!(team.drain_main_inbox()[0].id, "receipt-restored");
        assert!(team.main_message_attempts.is_empty());
    }
}
