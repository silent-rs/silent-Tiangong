use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 语音合成结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechResult {
    pub file_path: String,
    pub mime_type: String,
}

/// 语音识别结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeResult {
    pub text: String,
    pub audio_path: String,
    pub duration: Option<f64>,
}

/// @提及候选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MentionCandidate {
    pub value: String,
    pub label: String,
    pub kind: String,
    pub hint: String,
}

/// 会话列表项（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionListItem {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
}

impl SessionListItem {
    pub fn from_core(core_session: &tiangong_core::session::Session) -> Self {
        Self {
            id: core_session.id.clone(),
            title: core_session.title.clone(),
            created_at: core_session.created_at.clone(),
            updated_at: core_session.updated_at.clone(),
            message_count: core_session.messages.len(),
        }
    }
}

/// 运行状态快照（前端使用的完整快照）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSnapshotView {
    pub status: tiangong_types::RunStatus,
    pub summary: String,
    pub last_session_id: Option<String>,
    pub last_task_id: Option<String>,
    pub last_duration_ms: Option<u64>,
    pub last_result: Option<String>,
    pub last_plan: Option<String>,
    pub last_tool_result: Option<String>,
    pub last_error: Option<String>,
    pub last_usage: Option<tiangong_types::TokenUsage>,
    pub updated_at: String,
    pub messages: Vec<tiangong_types::Message>,
    pub input_draft: String,
    pub current_plan: Option<TaskPlan>,
    pub pending_session_ids: Vec<String>,
    pub approval_request_id: Option<String>,
}

impl RunSnapshotView {
    pub fn from_core_with_session(
        core_snapshot: &tiangong_core::runtime::RunSnapshot,
        messages: Vec<tiangong_types::Message>,
        input_draft: String,
        current_plan: Option<TaskPlan>,
        pending_session_ids: Vec<String>,
    ) -> Self {
        Self {
            status: core_snapshot.status,
            summary: core_snapshot.summary.clone(),
            last_session_id: core_snapshot.last_session_id.clone(),
            last_task_id: core_snapshot.last_task_id.clone(),
            last_duration_ms: core_snapshot.last_duration_ms,
            last_result: core_snapshot.last_result.clone(),
            last_plan: core_snapshot.last_plan.clone(),
            last_tool_result: core_snapshot.last_tool_result.clone(),
            last_error: core_snapshot.last_error.clone(),
            last_usage: core_snapshot.last_usage.clone(),
            updated_at: core_snapshot.updated_at.clone(),
            messages,
            input_draft,
            current_plan,
            pending_session_ids,
            approval_request_id: core_snapshot.approval_request_id.clone(),
        }
    }
}

/// 任务计划（前端使用的扁平视图）
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
    pub fn from_session_task_plan(session_plan: &tiangong_core::session::SessionTaskPlan) -> Self {
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
    pub fn from_session_step(step: &tiangong_core::session::SessionPlanExecutionStep) -> Self {
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

/// MCP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    pub enabled: bool,
}

impl McpServerView {
    pub fn from_core(core_server: &tiangong_core::agent_config::McpServerConfig) -> Self {
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
pub struct SkillView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub source_type: String,
}

impl SkillView {
    pub fn from_core(core_skill: &tiangong_core::agent_config::InstalledSkillConfig) -> Self {
        Self {
            id: core_skill.id.clone(),
            name: core_skill.name.clone(),
            version: core_skill.version.clone(),
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

/// Skill 安装前检查结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInspection {
    pub env_vars: Vec<String>,
    pub missing_env_vars: Vec<String>,
    pub dependencies: Vec<String>,
}

/// Server 配置（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfigView {
    pub host: String,
    pub port: u16,
    pub auth_token_masked: String,
    pub running: bool,
}

/// Connector 信息（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorInfoView {
    pub name: String,
    pub connector_type: String,
    pub enabled: bool,
}

impl ConnectorInfoView {
    pub fn from_config(config: &tiangong_connector::config::ConnectorConfig) -> Self {
        Self {
            name: config.name.clone(),
            connector_type: format!("{:?}", config.connector_type),
            enabled: config.enabled,
        }
    }
}

/// 模型能力（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilityInfo {
    pub key: String,
    pub display_name: String,
}

/// 能力可用性状态（基于当前配置快速检测）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityAvailabilityInfo {
    pub key: String,
    pub display_name: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routed_model: Option<String>,
}

/// Provider 连接配置（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfigView {
    pub base_url: String,
    pub api_key: String,
    pub timeout_ms: u64,
}

/// 单个模型配置（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntryView {
    pub provider: String,
    pub model: String,
    pub capabilities: Vec<String>,
    pub options: serde_json::Value,
}

/// 完整的三层模型配置（前端使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsConfigView {
    pub providers: HashMap<String, ProviderConfigView>,
    pub models: HashMap<String, ModelEntryView>,
    pub routing: HashMap<String, String>,
}

impl ModelsConfigView {
    pub fn from_core(config: &tiangong_core::models_config::ModelsConfig) -> Self {
        let providers = config
            .providers
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    ProviderConfigView {
                        base_url: v.base_url.clone(),
                        api_key: v.api_key.clone(),
                        timeout_ms: v.timeout_ms,
                    },
                )
            })
            .collect();

        let models = config
            .models
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    ModelEntryView {
                        provider: v.provider.clone(),
                        model: v.model.clone(),
                        capabilities: v
                            .capabilities
                            .iter()
                            .map(|c| serde_json::to_value(c).unwrap_or_default())
                            .map(|v| v.as_str().unwrap_or_default().to_string())
                            .collect(),
                        options: v.options.clone(),
                    },
                )
            })
            .collect();

        let routing = config
            .routing
            .iter()
            .map(|(k, v)| {
                let key = serde_json::to_value(k).unwrap_or_default();
                (key.as_str().unwrap_or_default().to_string(), v.clone())
            })
            .collect();

        Self {
            providers,
            models,
            routing,
        }
    }

    pub fn to_core(&self) -> tiangong_core::models_config::ModelsConfig {
        use tiangong_core::models_config::{
            ModelCapability, ModelEntry, ModelsConfig, ProviderConfig,
        };

        let providers = self
            .providers
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    ProviderConfig {
                        base_url: v.base_url.clone(),
                        api_key: v.api_key.clone(),
                        timeout_ms: v.timeout_ms,
                    },
                )
            })
            .collect();

        let models = self
            .models
            .iter()
            .map(|(k, v)| {
                let capabilities: Vec<ModelCapability> = v
                    .capabilities
                    .iter()
                    .filter_map(|c| {
                        let json_str = format!("\"{}\"", c);
                        serde_json::from_str(&json_str).ok()
                    })
                    .collect();
                (
                    k.clone(),
                    ModelEntry {
                        provider: v.provider.clone(),
                        model: v.model.clone(),
                        capabilities,
                        options: v.options.clone(),
                    },
                )
            })
            .collect();

        let routing = self
            .routing
            .iter()
            .filter_map(|(k, v)| {
                let json_str = format!("\"{}\"", k);
                let cap: ModelCapability = serde_json::from_str(&json_str).ok()?;
                Some((cap, v.clone()))
            })
            .collect();

        ModelsConfig {
            providers,
            models,
            routing,
        }
    }
}
