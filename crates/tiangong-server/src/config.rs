use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tiangong_connector::config::ConnectorConfig;

/// Server 模式配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 监听地址，默认 127.0.0.1
    pub host: String,
    /// 监听端口，默认 8080
    pub port: u16,
    /// API 认证 Token（为空则不鉴权）
    pub auth_token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            auth_token: None,
        }
    }
}

/// 获取用户 home 目录
fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| v != OsStr::new("")) {
        return Some(PathBuf::from(profile));
    }
    None
}

/// 配置文件路径: ~/.tiangong/server.json
fn config_file_path() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("server.json")
}

/// 从 ~/.tiangong/server.json 加载配置，文件不存在时返回默认值
pub fn load_server_config() -> ServerConfig {
    let path = config_file_path();
    if !path.exists() {
        return ServerConfig::default();
    }
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => ServerConfig::default(),
    }
}

/// 保存 Server 配置到 ~/.tiangong/server.json
pub fn save_server_config(config: &ServerConfig) -> anyhow::Result<()> {
    let path = config_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(config)?;
    fs::write(&path, content)?;
    Ok(())
}

impl ServerConfig {
    /// 返回脱敏后的 auth_token（仅显示前 4 位 + ****）
    pub fn masked_auth_token(&self) -> String {
        match &self.auth_token {
            None => "(未设置)".to_string(),
            Some(token) if token.trim().is_empty() => "(空)".to_string(),
            Some(token) if token.len() <= 4 => "****".to_string(),
            Some(token) => format!("{}****", &token[..4]),
        }
    }
}

/// Connector 配置文件结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorsFile {
    pub connectors: Vec<ConnectorConfig>,
}

/// Connector 配置文件路径: ~/.tiangong/connectors.json
fn connectors_file_path() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("connectors.json")
}

/// 从 ~/.tiangong/connectors.json 加载 Connector 配置
/// 文件不存在时返回空列表
pub fn load_connectors_config() -> Vec<ConnectorConfig> {
    let path = connectors_file_path();
    if !path.exists() {
        tracing::info!("Connector 配置文件不存在: {}", path.display());
        return Vec::new();
    }
    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<ConnectorsFile>(&content) {
            Ok(file) => file.connectors,
            Err(e) => {
                tracing::warn!("解析 Connector 配置失败: {e}");
                Vec::new()
            }
        },
        Err(e) => {
            tracing::warn!("读取 Connector 配置文件失败: {e}");
            Vec::new()
        }
    }
}

/// 保存 Connector 配置到 ~/.tiangong/connectors.json
pub fn save_connectors_config(connectors: &[ConnectorConfig]) -> anyhow::Result<()> {
    let path = connectors_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = ConnectorsFile {
        connectors: connectors.to_vec(),
    };
    let content = serde_json::to_string_pretty(&file)?;
    fs::write(&path, content)?;
    Ok(())
}

/// 设置指定 Connector 的启用状态并保存
pub fn set_connector_enabled(name: &str, enabled: bool) -> anyhow::Result<()> {
    let mut connectors = load_connectors_config();
    let found = connectors.iter_mut().find(|c| c.name == name);
    match found {
        Some(c) => {
            c.enabled = enabled;
            save_connectors_config(&connectors)?;
            Ok(())
        }
        None => Err(anyhow::anyhow!("未找到 Connector: {name}")),
    }
}
