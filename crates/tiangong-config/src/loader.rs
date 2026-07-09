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

use tiangong_core::models_config::ModelsConfig;
use tiangong_plugin_skill::SkillsConfig;

use crate::config::{ConnectorConfig, TiangongConfig};
use crate::io;

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
    let models = load_models_config(dir);
    let skills = load_json_config::<SkillsConfig>(dir, "skills.json").unwrap_or_default();
    let server = crate::config::load_server_config_from_dir(dir);
    let connectors = load_json_config::<ConnectorsFile>(dir, "connectors.json")
        .map(|f| f.connectors)
        .unwrap_or_default();

    // 自定义 Prompt：从 custom-prompt.md 加载（兼容 app.json 旧字段为空回退）
    let custom_system_prompt =
        io::load_custom_prompt_at(&dir.join("custom-prompt.md"), "").unwrap_or_default();

    // 首次安装：释放默认 context_windows.json
    io::ensure_context_windows(dir);

    // context_limit 在加载阶段按 chat model 从「同一目录」的 context_windows.json
    // 解析，避免 to_core_config 转换时误读默认 ~/.tiangong（自定义目录场景出错）。
    let chat_model = models
        .resolve_slot(tiangong_core::models_config::RoutingSlot::Chat)
        .map(|r| r.model.clone())
        .unwrap_or_default();
    let context_limit = if chat_model.is_empty() {
        tiangong_core::core_config::default_context_limit()
    } else {
        io::resolve_context_limit_at(dir, &chat_model)
    };

    // MCP 配置（mcp.json）与 capability 缓存由 tiangong-plugin-mcp 自管：
    // plugin 在 register 时加载缓存 + 启动后台调度器 + 预热探测，config 不再参与。

    TiangongConfig {
        models,
        skills,
        custom_system_prompt,
        context_limit,
        server,
        connectors,
        ..Default::default()
    }
}

/// 加载模型配置（仅从 `models.json` 读取；环境变量回退已移除）
fn load_models_config(dir: &Path) -> ModelsConfig {
    io::load_models_config_at(dir)
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
