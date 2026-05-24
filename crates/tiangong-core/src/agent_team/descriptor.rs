use serde::{Deserialize, Serialize};

/// Agent 状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// 空闲，等待消息
    Idle,
    /// 正在执行任务
    Running,
    /// 等待用户输入
    WaitingForUser,
    /// 等待文件锁
    WaitingForLock,
    /// 已销毁
    Terminated,
}

/// Agent 描述符
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDescriptor {
    /// 唯一标识（会话内唯一）
    pub agent_id: String,
    /// 角色标识（用于 @提及，如 "pm"、"dev"、"test"）
    pub role: String,
    /// 显示名称
    pub label: String,
    /// Agent 专属系统 prompt
    pub system_prompt: String,
    /// 可用工具列表（从主 Agent 工具集中选取）
    pub tools: Vec<String>,
    /// 当前状态
    pub status: AgentStatus,
}
