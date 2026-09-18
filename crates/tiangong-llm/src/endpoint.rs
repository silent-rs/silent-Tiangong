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
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
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
            headers: Default::default(),
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
            headers: resolved.headers,
            base_url: resolved.base_url,
            api_key: resolved.api_key,
            model: resolved.model,
            protocol: resolved.protocol,
            timeout_ms: resolved.timeout_ms,
            options: resolved.options,
        }
    }

    /// 端点是否可用于实际请求（base_url 与 model 均非空）。
    pub fn is_usable(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }

    /// 端点的模型标识：`base_url + model`（含协议）派生，**不落盘、不入配置**。
    ///
    /// 用途只有一个：判断两个端点是否指向同一个模型。不能只比 `model`——
    /// 不同平台常有同名 model id（如多家 OpenAI 兼容服务都叫 `qwen3-max`），
    /// 漏判会让切换后仍用旧端点、且跳过切换前的上下文整理。
    ///
    /// 与 `Session.model_ref`（用户在 `models` 注册表中选的条目名）是两回事：
    /// 后者是用户的选择策略并需要落盘，这里只是运行时端点身份。
    pub fn model_key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.protocol.as_str(),
            self.base_url.trim_end_matches('/'),
            self.model
        )
    }

    /// 是否指向同一个模型（判断「是否需要切换」的唯一依据）。
    pub fn is_same_model(&self, other: &Self) -> bool {
        self.model_key() == other.model_key()
    }

    /// 转为 [`crate::models_config::ResolvedModel`]，供 media facade 等需要路由解析结果的调用方使用。
    ///
    /// `ModelEndpoint` 与 `ResolvedModel` 字段一一对应（仅 `provider` 缺失，置空），
    /// 避免插件每次调用都走 `ModelsConfig::resolve_for_capability` 的完整路由解析。
    pub fn to_resolved(&self) -> crate::models_config::ResolvedModel {
        crate::models_config::ResolvedModel {
            headers: self.headers.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(base_url: &str, model: &str) -> ModelEndpoint {
        ModelEndpoint {
            base_url: base_url.to_string(),
            api_key: "sk-test".to_string(),
            model: model.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn 同一服务端同一模型视为同一模型() {
        let a = endpoint("https://api.example.com/v1", "gpt-4o");
        let b = endpoint("https://api.example.com/v1", "gpt-4o");
        assert!(a.is_same_model(&b));

        // 凭据、超时、headers 不属于模型身份，变化不应触发切换。
        let mut c = b.clone();
        c.api_key = "sk-rotated".to_string();
        c.timeout_ms = 5_000;
        assert!(a.is_same_model(&c));

        // 末尾斜杠只是书写差异。
        assert!(a.is_same_model(&endpoint("https://api.example.com/v1/", "gpt-4o")));
    }

    #[test]
    fn 同名模型不同服务端不得视为同一模型() {
        // 不同平台常有同名 model id：只比模型名会漏判切换，导致新端点
        // 未生效且跳过切换前的上下文整理。
        let a = endpoint("https://a.example.com/v1", "qwen3-max");
        let b = endpoint("https://b.example.com/v1", "qwen3-max");
        assert!(!a.is_same_model(&b));
        assert_ne!(a.model_key(), b.model_key());
    }

    #[test]
    fn 协议不同不得视为同一模型() {
        let mut a = endpoint("https://api.example.com/v1", "claude-sonnet");
        let mut b = a.clone();
        a.protocol = ProviderProtocol::OpenAiChatCompletions;
        b.protocol = ProviderProtocol::Anthropic;
        assert!(!a.is_same_model(&b));
    }

    #[test]
    fn 模型标识可直接从端点派生() {
        let endpoint = endpoint("https://api.example.com/v1", "gpt-4o");
        let key = endpoint.model_key();
        assert!(key.contains("https://api.example.com/v1"), "{key}");
        assert!(key.contains("gpt-4o"), "{key}");
        assert!(key.contains(endpoint.protocol.as_str()), "{key}");
        // 空端点也有稳定标识，不 panic。
        assert!(!ModelEndpoint::default().model_key().is_empty());
    }

    #[test]
    fn 端点可用性只取决于地址与模型名() {
        assert!(endpoint("https://api.example.com/v1", "m").is_usable());
        assert!(!endpoint("", "m").is_usable());
        assert!(!endpoint("https://api.example.com/v1", "  ").is_usable());
        assert!(!ModelEndpoint::default().is_usable());
    }

    #[test]
    fn 路由解析结果原样转为端点() {
        let resolved = crate::models_config::ResolvedModel {
            headers: Default::default(),
            provider: "p".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            timeout_ms: 60_000,
            protocol: ProviderProtocol::default(),
            model: "gpt-4o".to_string(),
            options: Value::Object(serde_json::Map::new()),
            context_window: None,
        };
        let endpoint = ModelEndpoint::from_resolved(resolved);
        assert_eq!(endpoint.base_url, "https://api.example.com/v1");
        assert_eq!(endpoint.model, "gpt-4o");
    }
}
