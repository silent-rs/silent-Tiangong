//! bot 制品清单（`bots-index.json`）模型与平台标识。
//!
//! bot 与主程序独立发版，每个 bot 在阿里云 OSS 保存自己的索引对象。
//! 主程序启动时合并所有可用索引，以渲染“可安装 bot 列表”。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::ConfigFieldSchema;

/// 各 bot 的独立索引端点。
pub const BOTS_INDEX_ENDPOINTS: &[&str] = &[
    "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com/bots-index/feishu.json",
    "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com/bots-index/weixin.json",
    "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com/bots-index/qq.json",
];

/// bots-index.json 顶层结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotsIndex {
    /// 清单格式版本。
    pub version: u32,
    /// 可用 bot 制品列表。
    pub bots: Vec<BotManifest>,
}

/// 单个 bot 制品的清单描述。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotManifest {
    /// 制品 id（标识 bot 平台，如 "feishu"；也是 [`BotConfig::artifact_id`] 的值）。
    pub id: String,
    /// 展示名称。
    pub name: String,
    /// 语义化版本。
    pub version: String,
    /// 描述。
    #[serde(default)]
    pub description: String,
    /// 配置字段 schema（前端据此渲染表单；权威来源是 bot `--describe` 上报）。
    #[serde(default)]
    pub config_schema: Vec<ConfigFieldSchema>,
    /// 各平台制品（key = `current_platform_key()` 返回值，如 "darwin-aarch64"）。
    pub platforms: BTreeMap<String, BotArtifact>,
    /// 兼容的最低主程序版本（semver）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_app_version: Option<String>,
}

/// 单个平台制品的下载信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotArtifact {
    /// 下载地址。
    pub url: String,
    /// SHA256 校验和（格式 "sha256:<hex>"）。
    pub checksum: String,
}

/// 当前平台的标识键（对齐 `tiangong-entry::update::current_platform_key`）。
///
/// 返回 `"{os}-{arch}"`，如 `darwin-aarch64` / `linux-x86_64` / `windows-x86_64`。
pub fn current_platform_key() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "unknown"
    };
    format!("{os}-{arch}")
}

impl BotManifest {
    /// 取当前平台的制品信息。
    pub fn current_artifact(&self) -> Option<&BotArtifact> {
        self.platforms.get(&current_platform_key())
    }
}
