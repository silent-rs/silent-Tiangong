use std::collections::HashMap;

use super::descriptor::{AgentDescriptor, AgentStatus};
use super::message_bus::{AgentInboxEntry, AgentMessage};
use tiangong_core::session::Session;

/// 会话级 Agent 注册表
pub struct AgentRegistry {
    agents: HashMap<String, AgentDescriptor>,
    /// agent_id → 收件箱
    inboxes: HashMap<String, Vec<AgentInboxEntry>>,
    /// agent_id → 独立 Session
    sessions: HashMap<String, Session>,
}

/// 团队工具执行前的完整收件箱快照，用于持久化失败时原样回滚。
#[derive(Clone)]
pub(crate) struct AgentInboxSnapshot {
    inboxes: HashMap<String, Vec<AgentInboxEntry>>,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            inboxes: HashMap::new(),
            sessions: HashMap::new(),
        }
    }

    /// 注册 Agent
    pub fn register(&mut self, descriptor: AgentDescriptor) {
        let session = Session::new(&descriptor.label);
        self.register_with_session(descriptor, session);
    }

    /// 注册 Agent 及其独立会话
    pub fn register_with_session(&mut self, descriptor: AgentDescriptor, session: Session) {
        let agent_id = descriptor.agent_id.clone();
        self.inboxes.insert(agent_id.clone(), Vec::new());
        self.sessions.insert(agent_id.clone(), session);
        self.agents.insert(agent_id, descriptor);
    }

    /// 注销 Agent
    pub fn unregister(&mut self, agent_id: &str) -> Option<AgentDescriptor> {
        self.inboxes.remove(agent_id);
        self.sessions.remove(agent_id);
        let mut descriptor = self.agents.remove(agent_id)?;
        descriptor.status = AgentStatus::Terminated;
        Some(descriptor)
    }

    /// 按 role 查找 Agent
    pub fn find_by_role(&self, role: &str) -> Option<&AgentDescriptor> {
        let role = role.trim().trim_start_matches('@');
        self.agents
            .values()
            .find(|a| a.role == role)
            .or_else(|| {
                self.agents
                    .values()
                    .find(|a| a.label.eq_ignore_ascii_case(role))
            })
            .or_else(|| {
                let alias = match role {
                    "developer" => "dev",
                    "tester" => "test",
                    "manager" | "project_manager" | "product_manager" => "pm",
                    _ => return None,
                };
                self.agents.values().find(|a| a.role == alias)
            })
    }

    /// 获取 Agent 描述符
    pub fn get(&self, agent_id: &str) -> Option<&AgentDescriptor> {
        self.agents.get(agent_id)
    }

    /// 获取 Agent 独立会话
    pub fn get_session(&self, agent_id: &str) -> Option<&Session> {
        self.sessions.get(agent_id)
    }

    /// 获取 Agent 独立会话（可变）
    pub fn get_session_mut(&mut self, agent_id: &str) -> Option<&mut Session> {
        self.sessions.get_mut(agent_id)
    }

    /// 替换 Agent 独立会话
    pub fn set_session(&mut self, agent_id: &str, session: Session) {
        if self.agents.contains_key(agent_id) {
            self.sessions.insert(agent_id.to_string(), session);
        }
    }

    /// 更新 Agent 状态
    pub fn update_status(&mut self, agent_id: &str, status: AgentStatus) {
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.status = status;
        }
    }

    /// 获取所有存活 Agent
    pub fn alive_agents(&self) -> Vec<&AgentDescriptor> {
        self.agents
            .values()
            .filter(|a| a.status != AgentStatus::Terminated)
            .collect()
    }

    /// 投递消息到 Agent 收件箱
    pub fn deliver_message(&mut self, agent_id: &str, message: AgentMessage) {
        self.deliver_inbox_entry(agent_id, AgentInboxEntry::plain(message));
    }

    /// 投递一条包含已准备内容的完整消息到 Agent 收件箱。
    pub fn deliver_inbox_entry(&mut self, agent_id: &str, entry: AgentInboxEntry) {
        if let Some(inbox) = self.inboxes.get_mut(agent_id) {
            if let Some(existing) = inbox
                .iter()
                .position(|existing| existing.message.id == entry.message.id)
            {
                inbox[existing] = entry;
                return;
            }
            inbox.push(entry);
        }
    }

    /// 按入队顺序取出一条待处理消息。
    ///
    /// 调度器一次只持有一个 entry；执行或持久化失败时必须调用
    /// [`Self::requeue_inbox_entry_front`] 把原 entry 放回队首。
    pub fn take_next_inbox_entry(&mut self, agent_id: &str) -> Option<AgentInboxEntry> {
        let inbox = self.inboxes.get_mut(agent_id)?;
        (!inbox.is_empty()).then(|| inbox.remove(0))
    }

    /// 把失败的消息原样放回队首，同时按稳定消息 ID 去重。
    pub fn requeue_inbox_entry_front(&mut self, agent_id: &str, entry: AgentInboxEntry) {
        if let Some(inbox) = self.inboxes.get_mut(agent_id) {
            inbox.retain(|existing| existing.message.id != entry.message.id);
            inbox.insert(0, entry);
        }
    }

    /// 克隆所有 Agent 收件箱。调用方应在持有 TeamContext 锁时使用。
    pub(crate) fn inbox_snapshot(&self) -> AgentInboxSnapshot {
        AgentInboxSnapshot {
            inboxes: self.inboxes.clone(),
        }
    }

    /// 恢复完整收件箱快照，撤销同一 TeamContext 临界区内的投递变更。
    pub(crate) fn restore_inbox_snapshot(&mut self, snapshot: AgentInboxSnapshot) {
        self.inboxes = snapshot.inboxes;
    }

    /// 返回相对快照新增的收件箱 entry，不改变当前队列。
    pub(crate) fn inbox_entries_added_since(
        &self,
        snapshot: &AgentInboxSnapshot,
    ) -> Vec<(String, AgentInboxEntry)> {
        let mut added = Vec::new();
        for (agent_id, inbox) in &self.inboxes {
            let previous_ids = snapshot
                .inboxes
                .get(agent_id)
                .into_iter()
                .flatten()
                .map(|entry| entry.message.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            added.extend(
                inbox
                    .iter()
                    .filter(|entry| !previous_ids.contains(entry.message.id.as_str()))
                    .cloned()
                    .map(|entry| (agent_id.clone(), entry)),
            );
        }
        added
    }

    pub fn remove_pending_source_message(&mut self, source_message_id: &str) {
        for inbox in self.inboxes.values_mut() {
            inbox.retain(|entry| entry.session_message_id.as_deref() != Some(source_message_id));
        }
    }

    pub fn has_pending_inbox(&self) -> bool {
        self.inboxes.values().any(|inbox| !inbox.is_empty())
    }

    pub fn has_pending_inbox_for(&self, agent_id: &str) -> bool {
        self.inboxes
            .get(agent_id)
            .map(|inbox| !inbox.is_empty())
            .unwrap_or(false)
    }

    pub fn agent_ids_with_pending_inbox(&self) -> Vec<String> {
        self.inboxes
            .iter()
            .filter(|(_, inbox)| !inbox.is_empty())
            .map(|(agent_id, _)| agent_id.clone())
            .collect()
    }

    /// 查看指定 Agent 尚未开始的全部投递 ID，不改变队列。
    pub fn pending_delivery_ids_for(&self, agent_id: &str) -> Vec<String> {
        self.inboxes
            .get(agent_id)
            .into_iter()
            .flatten()
            .map(|entry| entry.message.id.clone())
            .collect()
    }

    /// 查看所有尚未开始的投递 ID，不改变队列。
    pub fn all_pending_delivery_ids(&self) -> Vec<String> {
        self.inboxes
            .values()
            .flatten()
            .map(|entry| entry.message.id.clone())
            .collect()
    }

    /// 从所有收件箱移除指定的稳定投递 ID。
    pub fn remove_delivery_ids(&mut self, delivery_ids: &std::collections::BTreeSet<String>) {
        if delivery_ids.is_empty() {
            return;
        }
        for inbox in self.inboxes.values_mut() {
            inbox.retain(|entry| !delivery_ids.contains(&entry.message.id));
        }
    }

    /// 显式取消指定 Agent 尚未开始的持久直达投递，并返回稳定 delivery_id。
    pub fn cancel_pending_deliveries_for(&mut self, agent_id: &str) -> Vec<String> {
        let Some(inbox) = self.inboxes.get_mut(agent_id) else {
            return Vec::new();
        };
        let mut cancelled = Vec::new();
        inbox.retain(|entry| {
            if entry.session_message_id.is_some() {
                cancelled.push(entry.message.id.clone());
                false
            } else {
                true
            }
        });
        cancelled
    }

    /// 显式取消所有尚未开始的团队工作，并返回其中的持久直达投递 ID。
    ///
    /// 普通 Agent 间消息无需向 Core 确认，但全局取消同样必须清空，避免用户取消后
    /// 又在下一轮意外启动；持久直达消息则返回稳定 ID 供父会话提交取消。
    pub fn cancel_all_pending_deliveries(&mut self) -> Vec<String> {
        let mut cancelled = Vec::new();
        for inbox in self.inboxes.values_mut() {
            for entry in inbox.drain(..) {
                if entry.session_message_id.is_some() {
                    cancelled.push(entry.message.id);
                }
            }
        }
        cancelled
    }

    /// 清除尚未启动的所有 Agent 工作。用于显式“取消全部”；会话关闭不调用，
    /// 以便重启后恢复可靠投递。
    pub fn clear_pending_inboxes(&mut self) -> usize {
        self.inboxes
            .values_mut()
            .map(|inbox| {
                let count = inbox.len();
                inbox.clear();
                count
            })
            .sum()
    }

    /// 按 role 投递消息
    pub fn deliver_message_by_role(&mut self, role: &str, message: AgentMessage) {
        if let Some(agent) = self.find_by_role(role) {
            let agent_id = agent.agent_id.clone();
            self.deliver_message(&agent_id, message);
        }
    }

    /// 向所有存活 Agent 广播消息（排除指定 agent）
    pub fn broadcast(&mut self, message: AgentMessage, exclude: Option<&str>) {
        let targets: Vec<String> = self
            .alive_agents()
            .iter()
            .filter(|a| exclude != Some(a.agent_id.as_str()))
            .map(|a| a.agent_id.clone())
            .collect();
        for agent_id in targets {
            let msg = AgentMessage {
                to: agent_id.clone(),
                ..message.clone()
            };
            self.deliver_message(&agent_id, msg);
        }
    }

    /// 取出 Agent 收件箱中的所有消息
    pub fn drain_inbox(&mut self, agent_id: &str) -> Vec<AgentInboxEntry> {
        if let Some(inbox) = self.inboxes.get_mut(agent_id) {
            std::mem::take(inbox)
        } else {
            Vec::new()
        }
    }

    /// 已注册 Agent 数量（不含已销毁）
    pub fn alive_count(&self) -> usize {
        self.alive_agents().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MessagePriority;

    fn descriptor(agent_id: &str, role: &str) -> AgentDescriptor {
        AgentDescriptor {
            agent_id: agent_id.to_string(),
            role: role.to_string(),
            label: role.to_string(),
            system_prompt: "work".to_string(),
            tools: Vec::new(),
            status: AgentStatus::Idle,
        }
    }

    fn entry(id: &str, target: &str, source_message_id: Option<&str>) -> AgentInboxEntry {
        AgentInboxEntry {
            message: AgentMessage {
                id: id.to_string(),
                from: "main".to_string(),
                to: target.to_string(),
                content: id.to_string(),
                priority: MessagePriority::Normal,
                created_at: "2026-07-12 12:00:00".to_string(),
            },
            additional_content: Vec::new(),
            session_message_id: source_message_id.map(str::to_string),
        }
    }

    #[test]
    fn inbox_snapshot_reports_additions_and_restores_original_queue() {
        let mut registry = AgentRegistry::new();
        registry.register(descriptor("agent-dev", "dev"));
        registry.deliver_inbox_entry("agent-dev", entry("delivery-before", "agent-dev", None));
        let snapshot = registry.inbox_snapshot();

        registry
            .take_next_inbox_entry("agent-dev")
            .expect("快照后的队列应可变更");
        registry.deliver_inbox_entry("agent-dev", entry("delivery-after", "agent-dev", None));
        let added = registry.inbox_entries_added_since(&snapshot);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].0, "agent-dev");
        assert_eq!(added[0].1.message.id, "delivery-after");

        registry.restore_inbox_snapshot(snapshot);
        let restored = registry.drain_inbox("agent-dev");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].message.id, "delivery-before");
    }

    #[test]
    fn pending_delivery_ids_include_internal_and_user_deliveries() {
        let mut registry = AgentRegistry::new();
        registry.register(descriptor("agent-dev", "dev"));
        registry.register(descriptor("agent-test", "test"));
        registry.deliver_inbox_entry("agent-dev", entry("internal-1", "agent-dev", None));
        registry.deliver_inbox_entry("agent-dev", entry("user-1", "agent-dev", Some("message-1")));
        registry.deliver_inbox_entry("agent-test", entry("internal-2", "agent-test", None));

        assert_eq!(
            registry.pending_delivery_ids_for("agent-dev"),
            ["internal-1", "user-1"]
        );
        let mut all = registry.all_pending_delivery_ids();
        all.sort();
        assert_eq!(all, ["internal-1", "internal-2", "user-1"]);
    }
}
