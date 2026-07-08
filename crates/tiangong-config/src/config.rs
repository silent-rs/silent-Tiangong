//! TiangongConfig：完整应用配置

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tiangong_core::core_config::CoreConfig;
use tiangong_core::models_config::ModelsConfig;
use tiangong_core::permission::TrustMode;
use tiangong_plugin_skill::SkillsConfig;

/// Server 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 监听地址
    #[serde(default = "default_host")]
    pub host: String,
    /// 监听端口
    #[serde(default = "default_port")]
    pub port: u16,
    /// API 认证 Token
    #[serde(default)]
    pub auth_token: Option<String>,
    /// 上次退出时 Server 是否在运行，用于重启后自动拉起
    #[serde(default)]
    pub enabled: bool,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8080
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            auth_token: None,
            enabled: false,
        }
    }
}

/// 获取用户 home 目录（与 app_state::repository::utils 保持一致）。
fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = PathBuf::from(drive);
            buf.push(path);
            Some(buf)
        }
        _ => None,
    }
}

/// Server 配置文件路径：~/.tiangong/server.json
pub fn server_config_path() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("server.json")
}

/// 从指定目录加载 Server 配置，文件不存在时返回默认值。
pub fn load_server_config_from_dir(dir: &std::path::Path) -> ServerConfig {
    let path = dir.join("server.json");
    if !path.exists() {
        return ServerConfig::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => ServerConfig::default(),
    }
}

/// 从 ~/.tiangong/server.json 加载配置，文件不存在时返回默认值
pub fn load_server_config() -> ServerConfig {
    load_server_config_from_dir(&crate::loader::default_tiangong_dir())
}

/// 保存 Server 配置到指定目录的 server.json
pub fn save_server_config_to_dir(
    dir: &std::path::Path,
    config: &ServerConfig,
) -> anyhow::Result<()> {
    let path = dir.join("server.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    Ok(())
}

/// 保存 Server 配置到 ~/.tiangong/server.json
pub fn save_server_config(config: &ServerConfig) -> anyhow::Result<()> {
    save_server_config_to_dir(&crate::loader::default_tiangong_dir(), config)
}

impl ServerConfig {
    /// 返回脱敏后的 auth_token（显示前 4 位 + ****）
    pub fn masked_auth_token(&self) -> String {
        match &self.auth_token {
            None => "(未设置)".to_string(),
            Some(token) if token.trim().is_empty() => "(空)".to_string(),
            Some(token) if token.len() <= 4 => "****".to_string(),
            Some(token) => format!("{}****", &token[..4]),
        }
    }
}

/// 生成随机 Server Token。
///
/// 使用 scru128 生成具备时间序与随机性的标识，加 `tg_` 前缀。
/// 长度 `length` 指定 token 主体（不含前缀）的近似字节数；
/// 实际长度按 scru128 段落数向上取整。
pub fn generate_token(length: usize) -> String {
    let normalized = length.clamp(16, 256);
    let mut out = String::from("tg_");
    while out.len() - 3 < normalized {
        out.push_str(&scru128::new().to_string());
    }
    // 截断到目标长度（含前缀）
    if out.len() > 3 + normalized {
        out.truncate(3 + normalized);
    }
    out
}

/// Connector 类型
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum ConnectorType {
    #[default]
    Webhook,
    Telegram,
    Discord,
    Lark,
}

/// Connector 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    pub name: String,
    pub connector_type: ConnectorType,
    pub enabled: bool,
    pub settings: serde_json::Value,
}

/// 天工完整应用配置
///
/// 包含 Core 所需的配置（models/skills/trust_mode）
/// 以及应用层配置（server/connectors）。
///
/// MCP 配置已脱离（由 tiangong-plugin-mcp 自管 ~/.tiangong/mcp.json）。
#[derive(Debug, Clone, Default)]
pub struct TiangongConfig {
    // ===== Core 所需配置 =====
    /// LLM 模型配置
    pub models: ModelsConfig,
    /// Skill 配置
    pub skills: SkillsConfig,
    /// 权限信任模式
    pub trust_mode: TrustMode,
    /// 自定义系统 Prompt（从 custom-prompt.md 加载，注入 system prompt）
    pub custom_system_prompt: String,
    /// context window 上限（加载时按 chat model 从 context_windows.json 解析，
    /// 避免转换阶段再找目录——见 load_tiangong_config_from_dir）
    pub context_limit: usize,

    // ===== 应用层配置 =====
    /// Server 配置
    pub server: ServerConfig,
    /// Connector 配置列表
    pub connectors: Vec<ConnectorConfig>,
}

impl TiangongConfig {
    /// 转换为 CoreConfig（提取 Core 所需的最小子集）
    ///
    /// 将 ModelsConfig（3 层）解析为 LlmConfig（扁平端点）。
    /// 自定义 Prompt 来自加载时读取的 custom-prompt.md（见 load_tiangong_config_from_dir）。
    pub fn to_core_config(&self) -> CoreConfig {
        CoreConfig {
            llm: tiangong_core::core_config::LlmConfig::from_models_config(&self.models),
            trust_mode: self.trust_mode,
            default_trust_mode: self.trust_mode,
            custom_system_prompt: self.custom_system_prompt.clone(),
            reasoning_effort: "medium".to_string(),
            // context_limit 在加载时已按 chat model 从对应目录解析（见 loader）
            context_limit: self.context_limit,
        }
    }

    /// 创建 CoreConfigProvider（用于注入 TiangongCore）
    pub fn into_core_config_provider(self) -> tiangong_core::core_config::CoreConfigProvider {
        tiangong_core::core_config::CoreConfigProvider::new(self.to_core_config())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_config_default() {
        let config = ServerConfig::default();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 8080);
        assert_eq!(config.auth_token, None);
        assert!(!config.enabled);
    }

    #[test]
    fn server_config_serde_roundtrip_with_enabled() {
        let config = ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 9000,
            auth_token: Some("tg_secret".to_string()),
            enabled: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        // enabled 应被序列化
        assert!(json.contains("\"enabled\":true"));
        let parsed: ServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.host, config.host);
        assert_eq!(parsed.port, config.port);
        assert_eq!(parsed.auth_token, config.auth_token);
        assert!(parsed.enabled);
    }

    #[test]
    fn server_config_back_compat_without_enabled() {
        // 旧的 server.json（无 enabled 字段）应能反序列化，enabled 默认 false
        let legacy_json = r#"{"host":"127.0.0.1","port":8080,"auth_token":null}"#;
        let parsed: ServerConfig = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(parsed.host, "127.0.0.1");
        assert_eq!(parsed.port, 8080);
        assert!(!parsed.enabled);
    }

    #[test]
    fn masked_auth_token_variants() {
        let none = ServerConfig {
            auth_token: None,
            ..Default::default()
        };
        assert_eq!(none.masked_auth_token(), "(未设置)");

        let empty = ServerConfig {
            auth_token: Some("".to_string()),
            ..Default::default()
        };
        assert_eq!(empty.masked_auth_token(), "(空)");

        let short = ServerConfig {
            auth_token: Some("ab".to_string()),
            ..Default::default()
        };
        assert_eq!(short.masked_auth_token(), "****");

        let long = ServerConfig {
            auth_token: Some("tg_abcd1234567890".to_string()),
            ..Default::default()
        };
        assert_eq!(long.masked_auth_token(), "tg_a****");
    }

    #[test]
    fn generate_token_has_prefix_and_length() {
        let token = generate_token(32);
        assert!(token.starts_with("tg_"));
        // 主体长度应接近 32（向上取整到 scru128 段落边界）
        let body = &token[3..];
        assert!(body.len() >= 32, "body len = {}", body.len());
        assert!(token.len() < 3 + 32 + 40); // 不应远超目标
    }

    #[test]
    fn generate_token_clamps_extremes() {
        let too_short = generate_token(4);
        let too_long = generate_token(1000);
        // 过短被钳制到最小 16
        assert!(too_short.len() >= 3 + 16);
        // 过长被钳制到最大 256
        assert!(too_long.len() <= 3 + 256 + 40);
    }

    #[test]
    fn generate_token_unique() {
        let a = generate_token(32);
        let b = generate_token(32);
        assert_ne!(a, b, "连续生成的 token 不应相同");
    }

    #[test]
    fn to_core_config_carries_custom_system_prompt() {
        // P0 回归：to_core_config 必须携带 custom_system_prompt，
        // 否则 CLI/Server 启动的 Core 拿不到自定义 Prompt。
        let config = TiangongConfig {
            custom_system_prompt: "总是用简体中文".to_string(),
            ..Default::default()
        };
        let core = config.to_core_config();
        assert_eq!(core.custom_system_prompt, "总是用简体中文");
    }

    #[test]
    fn to_core_config_default_prompt_empty() {
        let config = TiangongConfig::default();
        let core = config.to_core_config();
        assert!(core.custom_system_prompt.is_empty());
    }

    #[test]
    fn load_custom_prompt_from_dir_reads_md_file() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        // 写入 custom-prompt.md
        fs::write(dir.path().join("custom-prompt.md"), "测试 Prompt 内容").unwrap();

        // 直接验证 load_custom_prompt_at 行为（与 load_tiangong_config_from_dir 一致）
        let prompt =
            crate::io::load_custom_prompt_at(&dir.path().join("custom-prompt.md"), "").unwrap();
        assert_eq!(prompt, "测试 Prompt 内容");
    }
}
