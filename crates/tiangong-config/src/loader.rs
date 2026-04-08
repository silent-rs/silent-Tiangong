//! 配置加载器：从磁盘文件构建 CoreConfig
//!
//! 默认配置目录：`~/.tiangong/`
//! 配置文件：
//! - `models.json` — 模型配置
//! - `mcp.json` — MCP 服务配置
//! - `skills.json` — Skill 配置
//! - `mcp-tools-cache.json` — MCP 能力缓存

use std::path::{Path, PathBuf};

use tiangong_core::agent_config::{McpConfig, SkillsConfig};
use tiangong_core::core_config::CoreConfig;
use tiangong_core::model::ModelProviderConfig;
use tiangong_core::models_config::ModelsConfig;

/// 获取默认配置目录（~/.tiangong/）
pub fn default_tiangong_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".tiangong")
}

/// 从默认目录加载完整配置
pub fn load_core_config() -> CoreConfig {
    load_core_config_from_dir(&default_tiangong_dir())
}

/// 从指定目录加载完整配置
pub fn load_core_config_from_dir(dir: &Path) -> CoreConfig {
    let models = load_models_config();
    let mcp = load_mcp_config(dir).unwrap_or_default();
    let skills = load_skills_config(dir).unwrap_or_default();

    // MCP 能力缓存加载 + 异步刷新
    let cache_path = dir.join("mcp-tools-cache.json");
    let _ = tiangong_core::mcp::load_mcp_capabilities_cache(&cache_path);
    let mcp_capabilities = tiangong_core::mcp::cached_active_tools();
    tiangong_core::mcp::refresh_mcp_capabilities_async(mcp.clone());

    CoreConfig {
        models,
        mcp,
        mcp_capabilities,
        skills,
        ..Default::default()
    }
}

/// 从环境变量加载最小配置（仅 LLM）
pub fn load_core_config_from_env() -> CoreConfig {
    CoreConfig {
        models: load_models_config(),
        ..Default::default()
    }
}

/// 加载模型配置
///
/// 优先从 models.json，回退到环境变量
pub fn load_models_config() -> ModelsConfig {
    let mut models = ModelsConfig::load();
    if models.is_empty() {
        let env_config = ModelProviderConfig::from_env();
        if !env_config.api_auth_token.is_empty() {
            models = ModelsConfig::from_legacy(&env_config);
        }
    }
    models
}

/// 从指定目录加载 MCP 配置
pub fn load_mcp_config(dir: &Path) -> Option<McpConfig> {
    let path = dir.join("mcp.json");
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<McpConfig>(&content) {
        Ok(config) => Some(config),
        Err(err) => {
            tracing::warn!("解析 mcp.json 失败：{err}");
            None
        }
    }
}

/// 从指定目录加载 Skills 配置
pub fn load_skills_config(dir: &Path) -> Option<SkillsConfig> {
    let path = dir.join("skills.json");
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<SkillsConfig>(&content) {
        Ok(config) => Some(config),
        Err(err) => {
            tracing::warn!("解析 skills.json 失败：{err}");
            None
        }
    }
}
