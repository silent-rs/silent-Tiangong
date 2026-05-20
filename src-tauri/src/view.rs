use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 语音合成结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechResult {
    pub file_path: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStatsView {
    pub current_tokens: usize,
    pub compression_threshold_tokens: usize,
    pub context_limit_tokens: usize,
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
    pub total_tokens: usize,
    pub active_agent_current_tokens: usize,
    pub active_agent_id: Option<String>,
    pub agent_current_tokens: HashMap<String, usize>,
    pub agent_token_usage: HashMap<String, tiangong_types::TokenUsage>,
}

impl TokenStatsView {
    pub fn from_session(core_session: &tiangong_core::session::Session) -> Self {
        let usage = core_session.total_usage();
        Self {
            current_tokens: core_session.current_tokens,
            compression_threshold_tokens: core_session.compression_threshold_tokens,
            context_limit_tokens: core_session.context_limit_tokens,
            total_prompt_tokens: usage.prompt_tokens,
            total_completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            active_agent_current_tokens: core_session.active_agent_current_tokens,
            active_agent_id: core_session.active_agent_id.clone(),
            agent_current_tokens: core_session.agent_current_tokens.clone(),
            agent_token_usage: core_session.agent_token_usage.clone(),
        }
    }
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
    pub token_stats: TokenStatsView,
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
        token_stats: TokenStatsView,
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
            token_stats,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDetailView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub entry: String,
    pub readme: String,
}

impl SkillDetailView {
    pub fn from_core(core_skill: &tiangong_core::skill::LoadedSkill) -> Self {
        Self {
            id: core_skill.manifest.id.clone(),
            name: core_skill.manifest.name.clone(),
            version: core_skill.manifest.version.clone(),
            description: if core_skill.manifest.description.is_empty() {
                None
            } else {
                Some(core_skill.manifest.description.clone())
            },
            enabled: core_skill.manifest.available,
            entry: core_skill.manifest.entry.clone(),
            readme: core_skill.readme.clone(),
        }
    }
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
    pub protocol: String,
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
                        protocol: v.protocol.as_str().to_string(),
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
            .filter(|(k, _)| {
                !matches!(
                    k,
                    tiangong_core::models_config::ModelCapability::Embedding
                        | tiangong_core::models_config::ModelCapability::Rerank
                )
            })
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
                        protocol: v.protocol.parse().unwrap_or_default(),
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
                if matches!(cap, ModelCapability::Embedding | ModelCapability::Rerank) {
                    return None;
                }
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

/// Memory 独立配置（前端使用）
///
/// 前端只保存对主 LLM Models 配置中 model key 的选择；后端在保存时
/// 将 model key 解析为 Memory runtime 需要的独立端点配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfigView {
    pub model_key: Option<String>,
    pub embedding_key: Option<String>,
    pub rerank_key: Option<String>,
    pub vector_mode: String,
}

impl MemoryConfigView {
    pub fn from_memory(
        config: &tiangong_memory::MemoryConfig,
        models: &tiangong_core::models_config::ModelsConfig,
    ) -> Self {
        Self {
            model_key: config
                .model
                .as_ref()
                .and_then(|endpoint| find_model_key_for_endpoint(models, endpoint)),
            embedding_key: config
                .embedding
                .as_ref()
                .and_then(|endpoint| find_embedding_key_for_endpoint(models, endpoint)),
            rerank_key: config
                .rerank
                .as_ref()
                .and_then(|endpoint| find_rerank_key_for_endpoint(models, endpoint)),
            vector_mode: memory_vector_mode_to_string(config.vector_mode).to_string(),
        }
    }

    pub fn to_memory(
        &self,
        models: &tiangong_core::models_config::ModelsConfig,
    ) -> Result<tiangong_memory::MemoryConfig, String> {
        Ok(tiangong_memory::MemoryConfig {
            model: self
                .model_key
                .as_deref()
                .filter(|key| !key.trim().is_empty())
                .map(|key| resolve_memory_llm(models, key))
                .transpose()?,
            embedding: self
                .embedding_key
                .as_deref()
                .filter(|key| !key.trim().is_empty())
                .map(|key| resolve_memory_embedding(models, key))
                .transpose()?,
            rerank: self
                .rerank_key
                .as_deref()
                .filter(|key| !key.trim().is_empty())
                .map(|key| resolve_memory_rerank(models, key))
                .transpose()?,
            vector_mode: memory_vector_mode_from_string(&self.vector_mode),
        })
    }
}

fn resolved_model_by_key(
    models: &tiangong_core::models_config::ModelsConfig,
    model_key: &str,
) -> Result<tiangong_core::models_config::ResolvedModel, String> {
    let model_entry = models
        .models
        .get(model_key)
        .ok_or_else(|| format!("模型不存在：{model_key}"))?;
    let provider = models.providers.get(&model_entry.provider).ok_or_else(|| {
        format!(
            "模型 {model_key} 引用的 Provider 不存在：{}",
            model_entry.provider
        )
    })?;

    Ok(tiangong_core::models_config::ResolvedModel {
        provider: model_entry.provider.clone(),
        base_url: provider.base_url.clone(),
        api_key: tiangong_core::models_config::ModelsConfig::resolve_api_key(&provider.api_key),
        timeout_ms: provider.timeout_ms,
        protocol: provider.protocol,
        model: model_entry.model.clone(),
        options: model_entry.options.clone(),
    })
}

fn resolve_memory_llm(
    models: &tiangong_core::models_config::ModelsConfig,
    model_key: &str,
) -> Result<tiangong_memory::MemoryLlmConfig, String> {
    let resolved = resolved_model_by_key(models, model_key)?;
    Ok(tiangong_memory::MemoryLlmConfig {
        provider_key: Some(resolved.provider),
        base_url: resolved.base_url,
        api_key: resolved.api_key,
        model: resolved.model,
        protocol: resolved.protocol,
        timeout_ms: resolved.timeout_ms,
    })
}

fn resolve_memory_embedding(
    models: &tiangong_core::models_config::ModelsConfig,
    model_key: &str,
) -> Result<tiangong_memory::MemoryEmbeddingConfig, String> {
    let resolved = resolved_model_by_key(models, model_key)?;
    let dimension = resolved
        .options
        .get("dimension")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("Embedding 模型 {model_key} 缺少 options.dimension"))?;
    Ok(tiangong_memory::MemoryEmbeddingConfig {
        provider_key: Some(resolved.provider),
        base_url: resolved.base_url,
        api_key: resolved.api_key,
        model: resolved.model,
        protocol: resolved.protocol,
        timeout_ms: resolved.timeout_ms,
        dimension,
    })
}

fn resolve_memory_rerank(
    models: &tiangong_core::models_config::ModelsConfig,
    model_key: &str,
) -> Result<tiangong_memory::MemoryRerankConfig, String> {
    let resolved = resolved_model_by_key(models, model_key)?;
    Ok(tiangong_memory::MemoryRerankConfig {
        provider_key: Some(resolved.provider),
        base_url: resolved.base_url,
        api_key: resolved.api_key,
        model: resolved.model,
        protocol: resolved.protocol,
        timeout_ms: resolved.timeout_ms,
    })
}

fn find_model_key_for_endpoint(
    models: &tiangong_core::models_config::ModelsConfig,
    endpoint: &tiangong_memory::MemoryLlmConfig,
) -> Option<String> {
    find_model_key(
        models,
        &endpoint.base_url,
        &endpoint.model,
        endpoint.protocol,
    )
}

fn find_embedding_key_for_endpoint(
    models: &tiangong_core::models_config::ModelsConfig,
    endpoint: &tiangong_memory::MemoryEmbeddingConfig,
) -> Option<String> {
    find_model_key(
        models,
        &endpoint.base_url,
        &endpoint.model,
        endpoint.protocol,
    )
}

fn find_rerank_key_for_endpoint(
    models: &tiangong_core::models_config::ModelsConfig,
    endpoint: &tiangong_memory::MemoryRerankConfig,
) -> Option<String> {
    find_model_key(
        models,
        &endpoint.base_url,
        &endpoint.model,
        endpoint.protocol,
    )
}

fn find_model_key(
    models: &tiangong_core::models_config::ModelsConfig,
    base_url: &str,
    model_name: &str,
    protocol: tiangong_core::model::ProviderProtocol,
) -> Option<String> {
    models.models.iter().find_map(|(model_key, model)| {
        let provider = models.providers.get(&model.provider)?;
        if provider.base_url == base_url
            && provider.protocol == protocol
            && model.model == model_name
        {
            Some(model_key.clone())
        } else {
            None
        }
    })
}

fn memory_vector_mode_to_string(mode: tiangong_memory::MemoryVectorMode) -> &'static str {
    match mode {
        tiangong_memory::MemoryVectorMode::Auto => "auto",
        tiangong_memory::MemoryVectorMode::Disabled => "disabled",
        tiangong_memory::MemoryVectorMode::EmbeddedLanceDb => "lancedb",
        tiangong_memory::MemoryVectorMode::ExternalQdrant => "external_qdrant",
    }
}

fn memory_vector_mode_from_string(value: &str) -> tiangong_memory::MemoryVectorMode {
    match value.trim().to_ascii_lowercase().as_str() {
        "disabled" => tiangong_memory::MemoryVectorMode::Disabled,
        "external_qdrant" | "qdrant" => tiangong_memory::MemoryVectorMode::ExternalQdrant,
        "lancedb" | "embedded_lancedb" => tiangong_memory::MemoryVectorMode::EmbeddedLanceDb,
        _ => tiangong_memory::MemoryVectorMode::Auto,
    }
}
