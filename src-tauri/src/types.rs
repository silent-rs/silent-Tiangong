use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 会话数据（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
}

impl Session {
    pub fn from_core(core_session: &tiangong_core::core::session::Session) -> Self {
        Self {
            id: core_session.id.clone(),
            title: core_session.title.clone(),
            created_at: core_session.created_at.clone(),
            updated_at: core_session.updated_at.clone(),
            message_count: core_session.messages.len(),
        }
    }
}

/// 消息数据（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub created_at: String,
}

impl Message {
    pub fn from_core(core_msg: &tiangong_core::core::session::Message) -> Self {
        Self {
            id: core_msg.id.clone(),
            role: format!("{:?}", core_msg.role),
            content: core_msg.content.clone(),
            reasoning_content: if core_msg.reasoning_content.is_empty() {
                None
            } else {
                Some(core_msg.reasoning_content.clone())
            },
            created_at: core_msg.created_at.clone(),
        }
    }
}

/// 运行状态快照（前端使用的完整快照）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub status: String,
    pub summary: String,
    pub last_session_id: Option<String>,
    pub last_task_id: Option<String>,
    pub last_duration_ms: Option<u64>,
    pub last_result: Option<String>,
    pub last_plan: Option<String>,
    pub last_tool_result: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
    /// 前端需要：当前会话的消息列表
    pub messages: Vec<Message>,
    /// 前端需要：当前输入草稿
    pub input_draft: String,
    /// 前端需要：当前执行计划
    pub current_plan: Option<TaskPlan>,
}

impl RunSnapshot {
    pub fn from_core_with_session(
        core_snapshot: &tiangong_core::core::runtime::RunSnapshot,
        messages: Vec<Message>,
        input_draft: String,
        current_plan: Option<TaskPlan>,
    ) -> Self {
        Self {
            status: format!("{:?}", core_snapshot.status).to_lowercase(),
            summary: core_snapshot.summary.clone(),
            last_session_id: core_snapshot.last_session_id.clone(),
            last_task_id: core_snapshot.last_task_id.clone(),
            last_duration_ms: core_snapshot.last_duration_ms,
            last_result: core_snapshot.last_result.clone(),
            last_plan: core_snapshot.last_plan.clone(),
            last_tool_result: core_snapshot.last_tool_result.clone(),
            last_error: core_snapshot.last_error.clone(),
            updated_at: core_snapshot.updated_at.clone(),
            messages,
            input_draft,
            current_plan,
        }
    }
}

/// 任务计划（匹配核心 TaskPlan 结构）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub id: String,
    pub objective: String,
    pub summary: String,
    pub items: Vec<PlanItem>,
    pub risks: Vec<String>,
    pub skill_hints: Vec<String>,
    pub mcp_hints: Vec<String>,
}

impl TaskPlan {
    pub fn from_core(core_plan: &tiangong_core::core::planner::TaskPlan) -> Self {
        Self {
            id: core_plan.id.clone(),
            objective: core_plan.objective.clone(),
            summary: core_plan.summary.clone(),
            items: core_plan.plans.iter().map(PlanItem::from_core).collect(),
            risks: core_plan.risks.clone(),
            skill_hints: core_plan.skill_hints.clone(),
            mcp_hints: core_plan.mcp_hints.clone(),
        }
    }

    pub fn from_session_task_plan(session_plan: &tiangong_core::core::session::SessionTaskPlan) -> Self {
        Self {
            id: session_plan.id.clone(),
            objective: session_plan.name.clone(),
            summary: session_plan.description.clone(),
            items: session_plan
                .execution_steps
                .iter()
                .map(PlanItem::from_session_step)
                .collect(),
            risks: vec![],
            skill_hints: vec![],
            mcp_hints: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: String,
    pub description: String,
    pub status: String,
    pub steps: Vec<PlanStep>,
}

impl PlanItem {
    pub fn from_core(core_item: &tiangong_core::core::planner::PlanItem) -> Self {
        Self {
            id: core_item.id.clone(),
            description: core_item.description.clone(),
            status: format!("{:?}", core_item.status),
            steps: core_item.execution_steps.iter().map(PlanStep::from_core).collect(),
        }
    }

    pub fn from_session_step(step: &tiangong_core::core::session::SessionPlanExecutionStep) -> Self {
        Self {
            id: step.id.clone(),
            description: step.description.clone(),
            status: format!("{:?}", step.status),
            steps: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub description: String,
    pub status: String,
    pub source: String,
}

impl PlanStep {
    pub fn from_core(core_step: &tiangong_core::core::planner::PlanStep) -> Self {
        Self {
            id: core_step.id.clone(),
            description: core_step.description.clone(),
            status: format!("{:?}", core_step.status),
            source: format!("{:?}", core_step.source),
        }
    }
}

/// MCP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    pub enabled: bool,
}

impl McpServer {
    pub fn from_core(core_server: &tiangong_core::core::agent_config::McpServerConfig) -> Self {
        Self {
            name: core_server.name.clone(),
            command: core_server.command.clone(),
            args: core_server.args.clone(),
            env: if core_server.env.is_empty() {
                None
            } else {
                Some(core_server.env.clone().into_iter().collect())
            },
            enabled: core_server.enabled,
        }
    }
}

/// Skill 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub source_type: String,
}

impl Skill {
    pub fn from_core(core_skill: &tiangong_core::core::agent_config::InstalledSkillConfig) -> Self {
        Self {
            id: core_skill.id.clone(),
            name: core_skill.name.clone(),
            description: if core_skill.description.is_empty() {
                None
            } else {
                Some(core_skill.description.clone())
            },
            enabled: core_skill.enabled,
            source_type: core_skill.source.kind.clone(),
        }
    }
}

/// 模型提供者配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: Option<String>,
    pub models: Vec<Model>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider_id: String,
}

/// 模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub api_auth_token: String,
    pub api_base_url: String,
    pub api_timeout_ms: String,
    pub api_model: String,
}
