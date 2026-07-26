//! bot 配置类型——bot 实例、字段 schema 与扫码字段抽象。
//!
//! 主程序不感知具体平台：所有 bot 用 `artifact_id`（制品 id 字符串，如
//! `"feishu"`）标识，加新平台只需发新 bot 制品 + 更新 bots-index.json。
//!
//! [`FieldType::Barcode`] 是通用扫码能力：任何 bot 可声明一个 barcode
//! 类型的操作入口，前端统一渲染二维码和轮询状态；扫码所得配置由 bot
//! 自行处理，主程序不读取。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::BotId;

/// 配置字段类型。
///
/// [`FieldType::Barcode`] 声明 bot 支持扫码配置，前端渲染二维码并轮询
/// [`crate::provision`] 状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldType {
    /// 普通字符串。
    String,
    /// 密钥（前端遮蔽显示）。
    Secret,
    /// 布尔开关。
    Boolean,
    /// 扫码配置入口（不作为普通配置值保存）。
    Barcode,
    /// 下拉选择。
    Select { options: Vec<String> },
}

/// 单个配置字段的 schema 描述。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFieldSchema {
    /// 字段键名（对应 [`BotConfig::config`] 的 key）。
    pub key: String,
    /// 展示标签。
    pub label: String,
    /// 字段类型。
    pub field_type: FieldType,
    /// 是否必填。
    pub required: bool,
    /// 该字段注入子进程的环境变量名（如 `TIANGONG_BOT_FEISHU_APP_ID`）。
    /// 主程序启动 bot 时按此映射注入。`None` 表示不注入（仅展示用字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// 默认值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// 帮助文案。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// Bot 制品通过 `--describe` 上报的完整描述。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotDescription {
    /// 描述协议版本。
    pub schema_version: u32,
    /// 制品 id，必须与安装清单或本地目录一致。
    pub artifact_id: String,
    /// 配置字段 schema。
    pub config_schema: Vec<ConfigFieldSchema>,
    /// 可选运行能力；旧 Bot 缺失时按无扩展能力处理。
    #[serde(default)]
    pub capabilities: BotCapabilities,
}

/// Bot 可选能力集合。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BotCapabilities {
    /// Bot 自带的 MCP 服务与配置生成能力。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<BotMcpCapability>,
}

/// Bot MCP 生成协议声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotMcpCapability {
    /// `bot --mcp generate` 输出协议版本。
    pub protocol_version: u32,
}

/// `bot --mcp generate` 输出的普通 MCP 注册配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotMcpConfig {
    pub schema_version: u32,
    pub name: String,
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// Bot 管理命令返回的通用推送目标视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushTargetView {
    /// Bot 生成的稳定目标 id，不暴露平台会话 id。
    pub target_id: String,
    /// 面向用户的目标名称。
    pub label: String,
    /// `direct` 或 `group`。
    pub kind: String,
    /// 是否已允许主动推送。
    pub enabled: bool,
    /// `ready`、`reply_window`、`unavailable` 或 `unknown`。
    pub availability: String,
    /// 最近一次收到该目标消息的本地时间。
    pub last_seen_at: String,
    /// 平台限制说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limitation: Option<String>,
}

/// `--push-target-list` 的标准输出。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushTargetList {
    pub targets: Vec<PushTargetView>,
}

/// 单个 bot 实例的配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    /// bot 实例名称（同时作为主键和目录名，如 `"feishu"`）。
    pub id: BotId,
    /// 制品 id（标识 bot 平台，如 `"feishu"`，来自 bots-index.json 的 manifest）。
    pub artifact_id: String,
    /// 是否启用（启用的 bot 在主程序启动时自动拉起）。
    #[serde(default)]
    pub enabled: bool,
    /// 配置键值（按 `--describe` 上报的 schema 声明的字段填写）。
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    /// 创建时间（`chrono::Local::now().naive_local()` 格式）。
    pub created_at: String,
    /// 更新时间。
    pub updated_at: String,
}

impl BotConfig {
    /// 从 config map 读取字符串字段。
    pub fn config_string(&self, key: &str) -> Option<String> {
        self.config
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}

/// bot 配置集合（持久化为 `~/.tiangong/bots/bots.json`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BotsConfig {
    /// 已注册的 bot 实例列表。
    #[serde(default)]
    pub bots: Vec<BotConfig>,
}

/// 注册 bot 的请求参数（前端表单提交）。
/// 注册 bot 的请求参数（前端表单提交）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterBotRequest {
    /// bot 实例名称（同时作为主键和目录名）。
    pub id: String,
    /// 制品 id（标识 bot 平台）。
    pub artifact_id: String,
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    #[serde(default)]
    pub enabled: bool,
}

/// 更新 bot 的请求参数（id 主键不变，就地更新配置）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateBotRequest {
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
}
