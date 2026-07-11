use std::collections::HashMap;

use crate::agent_team::descriptor::{AgentDescriptor, AgentStatus};
use crate::agent_team::message_bus::{AgentInboxEntry, AgentMessage};
use crate::session::Session;

/// 会话级 Agent 注册表
pub struct AgentRegistry {
    agents: HashMap<String, AgentDescriptor>,
    /// agent_id → 收件箱
    inboxes: HashMap<String, Vec<AgentInboxEntry>>,
    /// agent_id → 独立 Session
    sessions: HashMap<String, Session>,
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

    pub fn remove_pending_source_message(&mut self, source_message_id: &str) {
        for inbox in self.inboxes.values_mut() {
            inbox.retain(|entry| entry.session_message_id.as_deref() != Some(source_message_id));
        }
    }

    pub fn has_pending_inbox(&self) -> bool {
        self.inboxes.values().any(|inbox| !inbox.is_empty())
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
