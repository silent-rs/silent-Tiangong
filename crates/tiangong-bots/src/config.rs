//! bot 配置类型——bot 实例、字段 schema 与扫码字段抽象。
//!
//! 主程序不感知具体平台：所有 bot 用 `artifact_id`（制品 id 字符串，如
//! `"feishu"`）标识，加新平台只需发新 bot 制品 + 更新 bots-index.json。
//!
//! [`FieldType::Barcode`] 是通用扫码能力：任何 bot 可声明某个字段为 barcode
//! 类型，前端统一渲染二维码 + 轮询完成状态 + 回填凭证（首期按 Secret 走通，
//! 扫码渲染与协议由 [`crate::provision`] 提供，后续迭代补齐）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 配置字段类型。
///
/// [`FieldType::Barcode`] 声明该字段需要通过扫码授权获取（如飞书的
/// app_id/app_secret），前端渲染二维码并轮询 [`crate::provision`] 状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldType {
    /// 普通字符串。
    String,
    /// 密钥（前端遮蔽显示）。
    Secret,
    /// 布尔开关。
    Boolean,
    /// 扫码字段（通用扫码能力，首期前端按 Secret 输入框回退）。
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

/// 单个 bot 实例的配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    /// bot 实例名称（同时作为主键和目录名，如 `"feishu"`）。
    pub id: String,
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
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBotRequest {
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
}
