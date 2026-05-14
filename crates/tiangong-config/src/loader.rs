//! 配置加载器：从磁盘文件构建 TiangongConfig
//!
//! 默认配置目录：`~/.tiangong/`
//! 配置文件：
//! - `models.json` — 模型配置
//! - `mcp.json` — MCP 服务配置
//! - `skills.json` — Skill 配置
//! - `server.json` — Server 配置
//! - `connectors.json` — Connector 配置
//! - `mcp-tools-cache.json` — MCP 能力缓存

use std::path::{Path, PathBuf};

use tiangong_core::agent_config::{McpConfig, SkillsConfig};
use tiangong_core::model::ModelProviderConfig;
use tiangong_core::models_config::ModelsConfig;

use crate::config::{ConnectorConfig, ServerConfig, TiangongConfig};

/// Connector 配置文件结构
#[derive(serde::Deserialize)]
struct ConnectorsFile {
    #[serde(default)]
    connectors: Vec<ConnectorConfig>,
}

/// 获取默认配置目录（~/.tiangong/）
pub fn default_tiangong_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".tiangong")
}

/// 从默认目录加载完整配置
pub fn load_tiangong_config() -> TiangongConfig {
    load_tiangong_config_from_dir(&default_tiangong_dir())
}

/// 从指定目录加载完整配置
pub fn load_tiangong_config_from_dir(dir: &Path) -> TiangongConfig {
    let models = load_models_config();
    let mcp = load_json_config::<McpConfig>(dir, "mcp.json").unwrap_or_default();
    let skills = load_json_config::<SkillsConfig>(dir, "skills.json").unwrap_or_default();
    let server = load_json_config::<ServerConfig>(dir, "server.json").unwrap_or_default();
    let connectors = load_json_config::<ConnectorsFile>(dir, "connectors.json")
        .map(|f| f.connectors)
        .unwrap_or_default();

    // 首次安装：释放默认 context_windows.json
    ensure_context_windows(dir);

    // MCP 能力缓存加载 + 异步刷新
    let cache_path = dir.join("mcp-tools-cache.json");
    let _ = tiangong_core::mcp::load_mcp_capabilities_cache(&cache_path);
    let mcp_capabilities = tiangong_core::mcp::cached_active_tools();
    tiangong_core::mcp::refresh_mcp_capabilities_async(mcp.clone());

    TiangongConfig {
        models,
        mcp,
        mcp_capabilities,
        skills,
        server,
        connectors,
        ..Default::default()
    }
}

/// 加载模型配置（优先 models.json，回退环境变量）
fn load_models_config() -> ModelsConfig {
    let mut models = ModelsConfig::load();
    if models.is_empty() {
        let env_config = ModelProviderConfig::from_env();
        if !env_config.api_auth_token.is_empty() {
            models = ModelsConfig::from_legacy(&env_config);
        }
    }
    models
}

/// 通用 JSON 配置文件加载
fn load_json_config<T: serde::de::DeserializeOwned>(dir: &Path, filename: &str) -> Option<T> {
    let path = dir.join(filename);
    if !path.exists() {
        return None;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!("读取 {filename} 失败：{err}");
            return None;
        }
    };
    match serde_json::from_str::<T>(&content) {
        Ok(config) => Some(config),
        Err(err) => {
            tracing::warn!("解析 {filename} 失败：{err}");
            None
        }
    }
}

/// 如果用户目录下不存在 context_windows.json，则从内嵌默认内容创建
fn ensure_context_windows(dir: &Path) {
    let path = dir.join("context_windows.json");
    if path.exists() {
        return;
    }
    let default_content = tiangong_core::core_config::default_context_windows_json();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(err) = std::fs::write(&path, default_content) {
        tracing::warn!("写入 context_windows.json 失败：{err}");
    }
}
