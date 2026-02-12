use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    CreateChatCompletionRequestArgs,
};
use serde::{Deserialize, Serialize};
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use crate::core::session::{Message, MessageRole};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub session_title: String,
    pub user_input: String,
    pub context: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub text: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProviderConfig {
    #[serde(rename = "API_AUTH_TOKEN", default = "default_api_auth_token")]
    pub api_auth_token: String,
    #[serde(rename = "API_BASE_URL", default = "default_api_base_url")]
    pub api_base_url: String,
    #[serde(rename = "API_TIMEOUT_MS", default = "default_api_timeout_ms")]
    pub api_timeout_ms: String,
    #[serde(rename = "API_MODEL", default = "default_api_model")]
    pub api_model: String,
}

impl ModelProviderConfig {
    pub fn from_env() -> Self {
        let api_auth_token =
            std::env::var("API_AUTH_TOKEN").unwrap_or_else(|_| default_api_auth_token());
        let api_base_url = std::env::var("API_BASE_URL").unwrap_or_else(|_| default_api_base_url());
        let api_timeout_ms =
            std::env::var("API_TIMEOUT_MS").unwrap_or_else(|_| default_api_timeout_ms());
        let api_model = std::env::var("API_MODEL").unwrap_or_else(|_| default_api_model());
        Self {
            api_auth_token,
            api_base_url,
            api_timeout_ms,
            api_model,
        }
    }

    pub fn masked_auth_token(&self) -> String {
        if self.api_auth_token.trim().is_empty() {
            "(empty)".to_string()
        } else {
            "********".to_string()
        }
    }
}

pub trait ModelClient {
    fn api_base_url(&self) -> &str;
    fn api_timeout_ms(&self) -> &str;
    fn api_model(&self) -> &str;
    fn complete(&self, req: &ModelRequest) -> Result<ModelResponse>;
}

#[derive(Debug, Clone)]
pub struct SingleProviderClient {
    cfg: ModelProviderConfig,
}

impl SingleProviderClient {
    pub fn new(cfg: ModelProviderConfig) -> Self {
        Self { cfg }
    }

    pub fn list_models(cfg: &ModelProviderConfig) -> Result<Vec<String>> {
        let token = cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法更新模型列表"));
        }

        let timeout_ms = parse_timeout_ms(&cfg.api_timeout_ms)?;
        let api_base = normalize_api_base(&cfg.api_base_url)?;

        let config = OpenAIConfig::new()
            .with_api_key(token.to_string())
            .with_api_base(api_base);
        let client = Client::with_config(config);
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;

        let result = runtime.block_on(async {
            timeout(Duration::from_millis(timeout_ms), client.models().list()).await
        });

        let response = match result {
            Ok(Ok(resp)) => resp,
            Ok(Err(err)) => {
                let hint = build_sdk_error_hint(&err.to_string());
                return Err(anyhow!("更新模型列表失败：{err}{hint}"));
            }
            Err(_) => return Err(anyhow!("更新模型列表超时：{timeout_ms}ms")),
        };

        let mut models = response.data.into_iter().map(|m| m.id).collect::<Vec<_>>();
        models.sort();
        models.dedup();
        Ok(models)
    }
}

impl ModelClient for SingleProviderClient {
    fn api_base_url(&self) -> &str {
        &self.cfg.api_base_url
    }

    fn api_timeout_ms(&self) -> &str {
        &self.cfg.api_timeout_ms
    }

    fn api_model(&self) -> &str {
        &self.cfg.api_model
    }

    fn complete(&self, req: &ModelRequest) -> Result<ModelResponse> {
        let token = self.cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起模型请求"));
        }

        let timeout_ms = parse_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起模型请求"));
        }

        let api_base = normalize_api_base(&self.cfg.api_base_url)?;
        let messages = build_openai_messages(req)?;
        let request = CreateChatCompletionRequestArgs::default()
            .model(model.to_string())
            .messages(messages)
            .temperature(default_temperature())
            .max_tokens(default_max_tokens())
            .build()
            .context("构建 OpenAI 请求失败")?;

        let config = OpenAIConfig::new()
            .with_api_key(token.to_string())
            .with_api_base(api_base);
        let client = Client::with_config(config);

        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;

        let response = runtime.block_on(async {
            timeout(
                Duration::from_millis(timeout_ms),
                client.chat().create(request),
            )
            .await
        });

        let response = match response {
            Ok(Ok(resp)) => resp,
            Ok(Err(err)) => {
                let hint = build_sdk_error_hint(&err.to_string());
                return Err(anyhow!("OpenAI SDK 请求失败：{err}{hint}"));
            }
            Err(_) => return Err(anyhow!("模型请求超时：{timeout_ms}ms")),
        };

        let text = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("模型响应缺少文本内容"))?;

        let usage = response.usage.as_ref();
        let prompt_tokens = usage.map(|u| u.prompt_tokens as usize).unwrap_or(0);
        let completion_tokens = usage.map(|u| u.completion_tokens as usize).unwrap_or(0);
        let total_tokens = usage.map(|u| u.total_tokens as usize).unwrap_or(0);

        Ok(ModelResponse {
            text,
            usage: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            },
        })
    }
}

fn build_openai_messages(req: &ModelRequest) -> Result<Vec<ChatCompletionRequestMessage>> {
    let mut messages = Vec::new();
    let mut system_texts = vec![format!("当前会话：{}", req.session_title)];

    for msg in &req.context {
        match msg.role {
            MessageRole::System => {
                if !msg.content.trim().is_empty() {
                    system_texts.push(msg.content.clone());
                }
            }
            MessageRole::User => {
                messages.push(
                    ChatCompletionRequestUserMessageArgs::default()
                        .content(msg.content.clone())
                        .build()
                        .context("构建 user 消息失败")?
                        .into(),
                );
            }
            MessageRole::Assistant => {
                messages.push(
                    ChatCompletionRequestAssistantMessageArgs::default()
                        .content(msg.content.clone())
                        .build()
                        .context("构建 assistant 消息失败")?
                        .into(),
                );
            }
        }
    }

    messages.insert(
        0,
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_texts.join("\n"))
            .build()
            .context("构建 system 消息失败")?
            .into(),
    );

    messages.push(
        ChatCompletionRequestUserMessageArgs::default()
            .content(req.user_input.clone())
            .build()
            .context("构建当前 user 消息失败")?
            .into(),
    );

    Ok(messages)
}

fn normalize_api_base(base_url: &str) -> Result<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("API_BASE_URL 不能为空"));
    }

    let cleaned = trimmed.trim_end_matches('/');
    let cleaned = cleaned.strip_suffix("/chat/completions").unwrap_or(cleaned);
    if has_version_suffix(cleaned) {
        return Ok(cleaned.to_string());
    }
    Ok(format!("{cleaned}/v1"))
}

fn has_version_suffix(base_url: &str) -> bool {
    let suffix = base_url.rsplit('/').next().unwrap_or_default();
    let Some(version_num) = suffix.strip_prefix('v') else {
        return false;
    };
    !version_num.is_empty() && version_num.chars().all(|ch| ch.is_ascii_digit())
}

fn build_sdk_error_hint(error_text: &str) -> String {
    if error_text.contains("/v1/chat/completions") && error_text.contains("/v") {
        return "；请检查 API_BASE_URL，确保填写的是 OpenAI 兼容网关基地址（例如 .../v1 或 .../v4），不要重复拼接版本段".to_string();
    }
    if error_text.contains("expected struct ApiError") {
        return "；当前网关返回的错误结构非 OpenAI 标准格式，请确认 API_BASE_URL 是否为 OpenAI 兼容接口".to_string();
    }
    String::new()
}

fn default_max_tokens() -> u16 {
    std::env::var("API_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(2048)
}

fn default_temperature() -> f32 {
    std::env::var("API_TEMPERATURE")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.2)
}

fn default_api_auth_token() -> String {
    String::new()
}

fn default_api_base_url() -> String {
    "https://api.openai.com".to_string()
}

fn default_api_timeout_ms() -> String {
    "3000000".to_string()
}

fn default_api_model() -> String {
    "gpt-4o-mini".to_string()
}

fn parse_timeout_ms(raw: &str) -> Result<u64> {
    let timeout_ms = raw
        .trim()
        .parse::<u64>()
        .context("API_TIMEOUT_MS 解析失败，必须是毫秒数字")?;
    if timeout_ms == 0 {
        return Err(anyhow!("API_TIMEOUT_MS 必须大于 0"));
    }
    Ok(timeout_ms)
}
