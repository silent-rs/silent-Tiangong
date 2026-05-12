use std::collections::HashMap;

use crate::agent_team::descriptor::{AgentDescriptor, AgentStatus};
use crate::agent_team::message_bus::AgentMessage;

/// 会话级 Agent 注册表
pub struct AgentRegistry {
    agents: HashMap<String, AgentDescriptor>,
    /// agent_id → 收件箱
    inboxes: HashMap<String, Vec<AgentMessage>>,
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
        }
    }

    /// 注册 Agent
    pub fn register(&mut self, descriptor: AgentDescriptor) {
        let agent_id = descriptor.agent_id.clone();
        self.inboxes.insert(agent_id.clone(), Vec::new());
        self.agents.insert(agent_id, descriptor);
    }

    /// 注销 Agent
    pub fn unregister(&mut self, agent_id: &str) -> Option<AgentDescriptor> {
        self.inboxes.remove(agent_id);
        let mut descriptor = self.agents.remove(agent_id)?;
        descriptor.status = AgentStatus::Terminated;
        Some(descriptor)
    }

    /// 按 role 查找 Agent
    pub fn find_by_role(&self, role: &str) -> Option<&AgentDescriptor> {
        self.agents.values().find(|a| a.role == role)
    }

    /// 获取 Agent 描述符
    pub fn get(&self, agent_id: &str) -> Option<&AgentDescriptor> {
        self.agents.get(agent_id)
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
        if let Some(inbox) = self.inboxes.get_mut(agent_id) {
            inbox.push(message);
        }
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
    pub fn drain_inbox(&mut self, agent_id: &str) -> Vec<AgentMessage> {
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
