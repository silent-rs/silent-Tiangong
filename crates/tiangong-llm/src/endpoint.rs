//! 模型端点配置（扁平化的 Provider + Model 合并视图）。
//!
//! [`ModelEndpoint`] 是 core / plugin / client 层共享的最小端点契约：
//! base_url、api_key、model、protocol、timeout_ms、options。
//! 它由 [`crate::models_config::ResolvedModel`]（路由解析结果）经
//! [`ModelEndpoint::from_resolved`] 构造，也可经 [`ModelEndpoint::to_resolved`]
//! 转回路由结果类型供 media facade 等消费方使用。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::ProviderProtocol;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// 默认 context_window（模型名无法解析时的回退值）。
pub fn default_context_limit() -> usize {
    200_000
}

/// 模型端点配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEndpoint {
    /// API 基础 URL
    pub base_url: String,
    /// API 密钥
    pub api_key: String,
    /// 模型名称
    pub model: String,
    /// Provider 协议
    #[serde(default)]
    pub protocol: ProviderProtocol,
    /// 请求超时（毫秒）
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub options: Value,
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

impl Default for ModelEndpoint {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            protocol: ProviderProtocol::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            options: Value::Object(serde_json::Map::new()),
        }
    }
}

impl ModelEndpoint {
    /// 从 [`crate::models_config::ResolvedModel`] 构造（路由解析结果 → 扁平端点）。
    ///
    /// 供 plugin 从 `ModelsConfig::resolve_for_capability` 的结果构造端点，
    /// 不依赖 `LlmConfig` 的端点字段。
    pub fn from_resolved(resolved: crate::models_config::ResolvedModel) -> Self {
        Self {
            base_url: resolved.base_url,
            api_key: resolved.api_key,
            model: resolved.model,
            protocol: resolved.protocol,
            timeout_ms: resolved.timeout_ms,
            options: resolved.options,
        }
    }

    /// 转为 [`crate::models_config::ResolvedModel`]，供 media facade 等需要路由解析结果的调用方使用。
    ///
    /// `ModelEndpoint` 与 `ResolvedModel` 字段一一对应（仅 `provider` 缺失，置空），
    /// 避免插件每次调用都走 `ModelsConfig::resolve_for_capability` 的完整路由解析。
    pub fn to_resolved(&self) -> crate::models_config::ResolvedModel {
        crate::models_config::ResolvedModel {
            provider: String::new(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            timeout_ms: self.timeout_ms,
            protocol: self.protocol,
            model: self.model.clone(),
            options: self.options.clone(),
            context_window: None,
        }
    }
}
