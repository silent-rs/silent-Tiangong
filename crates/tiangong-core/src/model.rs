use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::session::{ContentBlock, MediaKind, Message, MessageRole};
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiangong_llm::message::{
    ChatMessage, MessageContent as LlmMessageContent, MessageRole as LlmMessageRole,
    ThinkingContent as LlmThinkingContent,
};
use tiangong_llm::provider::LlmProvider;
use tiangong_llm::providers::anthropic::{AnthropicConfig, AnthropicProvider};
use tiangong_llm::providers::deepseek::{DeepSeekConfig, DeepSeekProvider};
use tiangong_llm::providers::openai::{OpenAiResponsesConfig, OpenAiResponsesProvider};
use tiangong_llm::providers::openai_chatcompletions::{
    OpenAiChatCompletionsProvider, OpenAiChatConfig,
};
use tiangong_llm::request::ProviderRequest;
use tiangong_llm::response::{ProviderResponse, StopReason};
use tiangong_llm::stream::{ProviderStream, ProviderStreamEvent};
use tiangong_llm::tool::{
    ToolCall as LlmToolCall, ToolChoice as LlmToolChoice, ToolResult as LlmToolResult,
    ToolResultContent as LlmToolResultContent,
};
use tiangong_llm::usage::TokenUsageData;
use tokio::runtime::Builder as TokioRuntimeBuilder;

pub use tiangong_llm::ProviderProtocol;
pub use tiangong_llm::request::{ReasoningEffort, ThinkingConfig};
pub use tiangong_llm::tool::{ToolCall, ToolChoice, ToolSpec};
pub use tiangong_types::TokenUsage;

fn merge_stream_usage(current: &mut TokenUsageData, next: TokenUsageData) {
    current.prompt_tokens = current.prompt_tokens.max(next.prompt_tokens);
    current.completion_tokens = current.completion_tokens.max(next.completion_tokens);
    let computed_total = current.prompt_tokens + current.completion_tokens;
    current.total_tokens = current
        .total_tokens
        .max(next.total_tokens)
        .max(computed_total);
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub session_title: String,
    pub user_input: String,
    pub context: Vec<Message>,
    pub thinking: Option<ThinkingConfig>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub thinking_disabled: bool,
    /// 普通主模型请求不携带附件原始内容；只有显式的多模态解析工具会开启。
    pub include_media: bool,
}

impl ModelRequest {
    pub fn with_thinking_budget(mut self, budget_tokens: u32) -> Self {
        self.thinking = Some(ThinkingConfig { budget_tokens });
        self
    }
}

#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub text: String,
    pub reasoning_content: String,
    pub reasoning_signature: Option<String>,
    pub usage: TokenUsage,
    pub tool_calls: Vec<ToolCall>,
}

/// 向后兼容别名
pub type ModelFunctionResponse = ModelResponse;

#[derive(Debug, Clone, Default)]
pub struct ModelStreamChunk {
    pub content: String,
    pub reasoning_content: String,
    pub usage: Option<tiangong_llm::usage::TokenUsageData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProviderConfig {
    #[serde(rename = "API_AUTH_TOKEN", default = "default_api_auth_token")]
    pub api_auth_token: String,
    #[serde(rename = "API_BASE_URL", default = "default_api_base_url")]
    pub api_base_url: String,
    #[serde(rename = "API_TIMEOUT_MS", default = "default_api_timeout_ms")]
    pub api_timeout_ms: String,
    #[serde(rename = "API_PROTOCOL", default)]
    pub api_protocol: ProviderProtocol,
    #[serde(rename = "API_MODEL", default = "default_api_model")]
    pub api_model: String,
    #[serde(rename = "API_LITE_MODEL", default)]
    pub api_lite_model: String,
}

impl ModelProviderConfig {
    pub fn from_env() -> Self {
        let api_auth_token =
            std::env::var("API_AUTH_TOKEN").unwrap_or_else(|_| default_api_auth_token());
        let api_base_url = std::env::var("API_BASE_URL").unwrap_or_else(|_| default_api_base_url());
        let api_timeout_ms =
            std::env::var("API_TIMEOUT_MS").unwrap_or_else(|_| default_api_timeout_ms());
        let api_protocol = std::env::var("API_PROTOCOL")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default();
        let api_model = std::env::var("API_MODEL").unwrap_or_else(|_| default_api_model());
        let api_lite_model = std::env::var("API_LITE_MODEL").unwrap_or_default();
        Self {
            api_auth_token,
            api_base_url,
            api_timeout_ms,
            api_protocol,
            api_model,
            api_lite_model,
        }
    }

    /// 返回轻量级模型名称，如果未配置则回退到主模型
    pub fn lite_model(&self) -> &str {
        let lite = self.api_lite_model.trim();
        if lite.is_empty() {
            &self.api_model
        } else {
            lite
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
    /// 流式调用，通过 on_delta 实时回调每个 chunk（thinking + content）
    fn complete_stream(
        &self,
        req: &ModelRequest,
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelResponse> {
        let resp = self.complete(req)?;
        if !resp.reasoning_content.is_empty() {
            on_delta(&ModelStreamChunk {
                content: String::new(),
                reasoning_content: resp.reasoning_content.clone(),
                usage: None,
            });
        }
        if !resp.text.is_empty() {
            on_delta(&ModelStreamChunk {
                content: resp.text.clone(),
                reasoning_content: String::new(),
                usage: None,
            });
        }
        Ok(resp)
    }
    fn complete_with_functions(
        &self,
        req: &ModelRequest,
        _functions: &[ToolSpec],
    ) -> Result<ModelFunctionResponse> {
        self.complete(req)
    }
    /// 流式函数调用，通过 on_delta 实时回调 thinking chunk
    fn complete_with_functions_stream(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        let resp = self.complete_with_functions(req, functions)?;
        if !resp.reasoning_content.is_empty() {
            on_delta(&ModelStreamChunk {
                content: String::new(),
                reasoning_content: resp.reasoning_content.clone(),
                usage: None,
            });
        }
        if !resp.text.is_empty() {
            on_delta(&ModelStreamChunk {
                content: resp.text.clone(),
                reasoning_content: String::new(),
                usage: None,
            });
        }
        Ok(resp)
    }
}

/// 重试回调类型：(attempt, max_attempts, delay_ms, error_text)
pub type OnRetryCallback = Arc<dyn Fn(u32, u32, u64, &str) + Send + Sync>;

#[derive(Clone)]
pub struct SingleProviderClient {
    cfg: ModelProviderConfig,
    /// 重试时的回调通知（可选）
    on_retry: Option<OnRetryCallback>,
}

impl std::fmt::Debug for SingleProviderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleProviderClient")
            .field("cfg", &self.cfg)
            .field("on_retry", &self.on_retry.is_some())
            .finish()
    }
}

impl SingleProviderClient {
    pub fn new(cfg: ModelProviderConfig) -> Self {
        Self {
            cfg,
            on_retry: None,
        }
    }

    /// 设置重试回调
    pub fn with_on_retry(mut self, cb: OnRetryCallback) -> Self {
        self.on_retry = Some(cb);
        self
    }

    pub async fn list_models_async(cfg: &ModelProviderConfig) -> Result<Vec<String>> {
        let token = cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法更新模型列表"));
        }

        let timeout_ms = parse_timeout_ms(&cfg.api_timeout_ms)?;
        let mut models = if cfg.api_protocol == ProviderProtocol::Anthropic {
            let provider = build_anthropic_provider_from_config(cfg, timeout_ms, None)?;
            provider
                .list_models()
                .await
                .map(|items| items.into_iter().map(|item| item.id).collect::<Vec<_>>())
                .map_err(map_llm_error)?
        } else if cfg.api_protocol == ProviderProtocol::DeepSeek {
            let provider = build_deepseek_provider_from_config(cfg, timeout_ms, None)?;
            provider
                .list_models()
                .await
                .map(|items| items.into_iter().map(|item| item.id).collect::<Vec<_>>())
                .map_err(map_llm_error)?
        } else if cfg.api_protocol == ProviderProtocol::OpenAi {
            let provider = build_openai_responses_provider_from_config(cfg, timeout_ms, None)?;
            provider
                .list_models()
                .await
                .map(|items| items.into_iter().map(|item| item.id).collect::<Vec<_>>())
                .map_err(map_llm_error)?
        } else {
            let provider = build_openai_provider_from_config(cfg, timeout_ms, None)?;
            provider
                .list_models()
                .await
                .map(|items| items.into_iter().map(|item| item.id).collect::<Vec<_>>())
                .map_err(map_llm_error)?
        };
        models.sort();
        models.dedup();
        Ok(models)
    }

    pub fn list_models(cfg: &ModelProviderConfig) -> Result<Vec<String>> {
        let token = cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法更新模型列表"));
        }

        let timeout_ms = parse_timeout_ms(&cfg.api_timeout_ms)?;
        if cfg.api_protocol == ProviderProtocol::Anthropic {
            let provider = build_anthropic_provider_from_config(cfg, timeout_ms, None)?;
            let runtime = TokioRuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .context("初始化异步运行时失败")?;
            let mut models = runtime
                .block_on(provider.list_models())
                .map(|items| items.into_iter().map(|item| item.id).collect::<Vec<_>>())
                .map_err(map_llm_error)?;
            models.sort();
            models.dedup();
            return Ok(models);
        }
        if cfg.api_protocol == ProviderProtocol::DeepSeek {
            let provider = build_deepseek_provider_from_config(cfg, timeout_ms, None)?;
            let runtime = TokioRuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .context("初始化异步运行时失败")?;
            let mut models = runtime
                .block_on(provider.list_models())
                .map(|items| items.into_iter().map(|item| item.id).collect::<Vec<_>>())
                .map_err(map_llm_error)?;
            models.sort();
            models.dedup();
            return Ok(models);
        }
        if cfg.api_protocol == ProviderProtocol::OpenAi {
            let provider = build_openai_responses_provider_from_config(cfg, timeout_ms, None)?;
            let runtime = TokioRuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .context("初始化异步运行时失败")?;
            let mut models = runtime
                .block_on(provider.list_models())
                .map(|items| items.into_iter().map(|item| item.id).collect::<Vec<_>>())
                .map_err(map_llm_error)?;
            models.sort();
            models.dedup();
            return Ok(models);
        }
        let provider = build_openai_provider_from_config(cfg, timeout_ms, None)?;
        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;
        let mut models = runtime
            .block_on(provider.list_models())
            .map(|items| items.into_iter().map(|item| item.id).collect::<Vec<_>>())
            .map_err(map_llm_error)?;
        models.sort();
        models.dedup();
        Ok(models)
    }

    pub fn protocol(&self) -> ProviderProtocol {
        self.cfg.api_protocol
    }

    fn build_anthropic_provider(&self, timeout_ms: u64) -> Result<AnthropicProvider> {
        build_anthropic_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())
    }

    /// 根据协议构建 OpenAI 变体 provider（Responses 或 Chat Completions）。
    ///
    /// 用于非流式 complete 路径：Anthropic/DeepSeek 已在各方法内单独处理，
    /// 此处仅覆盖 OpenAI 两种变体。
    fn build_openai_variant_provider(&self, timeout_ms: u64) -> Result<Box<dyn LlmProvider>> {
        match self.protocol() {
            ProviderProtocol::OpenAi => Ok(Box::new(build_openai_responses_provider_from_config(
                &self.cfg,
                timeout_ms,
                self.on_retry.clone(),
            )?)),
            // Chat Completions（含旧 openai_compatible 别名）。
            ProviderProtocol::OpenAiChatCompletions => Ok(Box::new(
                build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?,
            )),
            // 其它协议（Anthropic/DeepSeek）不应进入此方法，给出明确错误。
            protocol => Err(anyhow!(
                "build_openai_variant_provider 不支持协议 {}",
                protocol.as_str()
            )),
        }
    }

    fn build_provider_dispatch(&self, timeout_ms: u64) -> Result<ProviderDispatch> {
        match self.protocol() {
            ProviderProtocol::Anthropic => Ok(ProviderDispatch::Anthropic(Box::new(
                self.build_anthropic_provider(timeout_ms)?,
            ))),
            ProviderProtocol::OpenAi => Ok(ProviderDispatch::OpenAiResponses(Box::new(
                build_openai_responses_provider_from_config(
                    &self.cfg,
                    timeout_ms,
                    self.on_retry.clone(),
                )?,
            ))),
            // Chat Completions（含旧 openai_compatible 别名）。
            ProviderProtocol::OpenAiChatCompletions => Ok(ProviderDispatch::OpenAi(Box::new(
                build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?,
            ))),
            ProviderProtocol::DeepSeek => {
                let mut config = DeepSeekConfig::new(self.cfg.api_auth_token.trim().to_string());
                config.base_url = if self.cfg.api_base_url.trim().is_empty() {
                    None
                } else {
                    Some(self.cfg.api_base_url.clone())
                };
                config.timeout = Duration::from_millis(timeout_ms);
                config.max_retries = MAX_RETRIES;
                config.retry_notifier = self.on_retry.clone();
                Ok(ProviderDispatch::DeepSeek(Box::new(
                    DeepSeekProvider::from_config(config)?,
                )))
            }
        }
    }

    fn block_on_llm<F, T>(&self, future: F) -> Result<T>
    where
        F: std::future::Future<Output = std::result::Result<T, tiangong_llm::error::LlmError>>
            + Send,
        T: Send,
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            return std::thread::scope(|scope| {
                let handle = scope.spawn(move || {
                    let runtime = TokioRuntimeBuilder::new_current_thread()
                        .enable_all()
                        .build()
                        .context("初始化异步运行时失败")?;
                    runtime.block_on(future).map_err(map_llm_error)
                });
                handle.join().map_err(|_| anyhow!("LLM 请求线程 panic"))?
            });
        }

        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;
        runtime.block_on(future).map_err(map_llm_error)
    }

    fn complete_anthropic(&self, req: &ModelRequest) -> Result<ModelResponse> {
        self.complete_with_functions_anthropic(req, &[])
    }

    fn complete_with_functions_anthropic(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
    ) -> Result<ModelFunctionResponse> {
        self.complete_with_functions_anthropic_with_tool_choice(req, functions, None)
    }

    fn complete_with_functions_anthropic_with_tool_choice(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
        tool_choice: Option<ToolChoice>,
    ) -> Result<ModelFunctionResponse> {
        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起 Anthropic 请求"));
        }

        let provider = self.build_anthropic_provider(timeout_ms)?;
        let request = build_provider_request(req, model, MAX_TOKENS_MAIN, functions, tool_choice);
        let response = self.block_on_llm(provider.complete(request))?;
        convert_provider_response_to_function_response(response)
    }

    fn complete_lite_anthropic(&self, prompt: &str) -> Result<String> {
        let timeout_ms = 120_000u64;
        let model = self.cfg.lite_model().trim();
        if model.is_empty() {
            return Err(anyhow!(
                "API_MODEL 不能为空，无法发起 Anthropic 轻量模型请求"
            ));
        }

        let provider = self.build_anthropic_provider(timeout_ms)?;
        let request = ProviderRequest {
            model: model.to_string(),
            system: Some(
                "你是会话标题生成助手。根据用户输入生成简洁的标题，要求：\
1. 标题不超过10个汉字\
2. 直接返回标题，不要任何解释或额外文字\
3. 标题要概括性强，简洁明了"
                    .to_string(),
            ),
            messages: vec![ChatMessage::text(LlmMessageRole::User, prompt)],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: MAX_TOKENS_LITE,
            temperature: Some(0.3),
            top_p: None,
            stop_sequences: Vec::new(),
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
        };
        let response = self.block_on_llm(provider.complete(request))?;
        let text = strip_think_tags(&collect_provider_text(&response))
            .trim()
            .to_string();
        Ok(text)
    }

    pub fn complete_stream_with_callback<F>(
        &self,
        req: &ModelRequest,
        mut on_delta: F,
    ) -> Result<ModelResponse>
    where
        F: FnMut(&ModelStreamChunk),
    {
        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起流式模型请求"));
        }
        let request = build_provider_request(req, model, MAX_TOKENS_MAIN, &[], None);
        let provider = self.build_provider_dispatch(timeout_ms)?;
        consume_provider_stream(provider, request, &mut on_delta)
    }

    /// 流式函数调用：实时输出 thinking，同时累积 tool_calls
    pub fn complete_with_functions_stream_impl(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        self.complete_with_functions_stream_impl_with_tool_choice(req, functions, None, on_delta)
    }

    pub fn complete_with_functions_stream_impl_with_tool_choice(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
        tool_choice: Option<ToolChoice>,
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起流式工具模型请求"));
        }
        let request = build_provider_request(req, model, MAX_TOKENS_MAIN, functions, tool_choice);
        let provider = self.build_provider_dispatch(timeout_ms)?;
        convert_stream_to_function_response(provider, request, on_delta)
    }

    /// 使用轻量级模型完成简单任务（如会话名称生成）
    /// 如果未配置轻量级模型，则使用主模型
    /// 该方法使用更短的超时时间和较低温度以获得更确定的结果
    pub fn complete_lite(&self, prompt: &str) -> Result<String> {
        if self.protocol() == ProviderProtocol::Anthropic {
            return self.complete_lite_anthropic(prompt);
        }
        let timeout_ms = 120_000u64;
        let model = self.cfg.lite_model().trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起轻量级模型请求"));
        }
        let provider = self.build_openai_variant_provider(timeout_ms)?;
        let request = ProviderRequest {
            model: model.to_string(),
            system: Some(
                "你是会话标题生成助手。根据用户输入生成简洁的标题，要求：\
                    1. 标题不超过10个汉字\
                    2. 直接返回标题，不要任何解释或额外文字\
                    3. 标题要概括性强，简洁明了"
                    .to_string(),
            ),
            messages: vec![ChatMessage::text(LlmMessageRole::User, prompt)],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: MAX_TOKENS_LITE,
            temperature: Some(0.3),
            top_p: None,
            stop_sequences: Vec::new(),
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
        };
        let response = self.block_on_llm(provider.complete(request))?;
        Ok(collect_provider_text(&response).trim().to_string())
    }

    /// 使用自定义 system prompt 的轻量级模型调用
    ///
    /// 适用于检索策略判断、意图分析等简单分类任务。
    pub fn complete_lite_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let timeout_ms = 120_000u64;
        let model = self.cfg.lite_model().trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起轻量级模型请求"));
        }
        let request = ProviderRequest {
            model: model.to_string(),
            system: Some(system.to_string()),
            messages: vec![ChatMessage::text(LlmMessageRole::User, prompt)],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: MAX_TOKENS_LITE,
            temperature: Some(0.1),
            top_p: None,
            stop_sequences: Vec::new(),
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
        };
        if self.protocol() == ProviderProtocol::Anthropic {
            let provider = self.build_anthropic_provider(timeout_ms)?;
            let response = self.block_on_llm(provider.complete(request))?;
            return Ok(strip_think_tags(&collect_provider_text(&response))
                .trim()
                .to_string());
        }
        let provider = self.build_openai_variant_provider(timeout_ms)?;
        let response = self.block_on_llm(provider.complete(request))?;
        Ok(collect_provider_text(&response).trim().to_string())
    }
    /// 真正的 async 流式函数调用。
    ///
    /// 通过 `chunk_tx` 实时发送每个 token chunk，完成后返回 `ModelFunctionResponse`。
    /// 该方法持有 `self`（owned），可直接在 `tokio::spawn` 里使用，future 是 `Send + 'static`。
    /// 取消 JoinHandle 后 HTTP 流会随 future drop 而断开。
    pub async fn stream_function_calls(
        self,
        req: ModelRequest,
        functions: Vec<ToolSpec>,
        chunk_tx: tokio::sync::mpsc::UnboundedSender<ModelStreamChunk>,
    ) -> Result<ModelFunctionResponse> {
        self.stream_function_calls_with_tool_choice(req, functions, None, chunk_tx)
            .await
    }

    pub async fn stream_function_calls_with_tool_choice(
        self,
        req: ModelRequest,
        functions: Vec<ToolSpec>,
        tool_choice: Option<ToolChoice>,
        chunk_tx: tokio::sync::mpsc::UnboundedSender<ModelStreamChunk>,
    ) -> Result<ModelFunctionResponse> {
        let fallback_client = self.clone();
        let fallback_req = req.clone();
        let fallback_functions = functions.clone();
        let fallback_tool_choice = tool_choice.clone();
        let fallback_tx = chunk_tx.clone();

        match self
            .stream_function_calls_streaming(req, functions, tool_choice, chunk_tx)
            .await
        {
            Ok(response) => Ok(response),
            Err(err) => {
                if let Some(on_retry) = &fallback_client.on_retry {
                    on_retry(1, MAX_RETRIES, 0, &err.to_string());
                }
                let response = tokio::task::spawn_blocking(move || {
                    fallback_client.complete_with_functions_with_tool_choice(
                        &fallback_req,
                        &fallback_functions,
                        fallback_tool_choice,
                    )
                })
                .await
                .context("流式失败后回退非流式调用失败")??;
                if !response.reasoning_content.is_empty() {
                    let _ = fallback_tx.send(ModelStreamChunk {
                        content: String::new(),
                        reasoning_content: response.reasoning_content.clone(),
                        usage: None,
                    });
                }
                if !response.text.is_empty() {
                    let _ = fallback_tx.send(ModelStreamChunk {
                        content: response.text.clone(),
                        reasoning_content: String::new(),
                        usage: None,
                    });
                }
                Ok(response)
            }
        }
    }

    async fn stream_function_calls_streaming(
        self,
        req: ModelRequest,
        functions: Vec<ToolSpec>,
        tool_choice: Option<ToolChoice>,
        chunk_tx: tokio::sync::mpsc::UnboundedSender<ModelStreamChunk>,
    ) -> Result<ModelFunctionResponse> {
        fn looks_like_complete_json(raw: &str) -> bool {
            let trimmed = raw.trim();
            (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        }

        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim().to_string();
        if model.is_empty() {
            return Err(anyhow!(
                "API_MODEL 不能为空，无法发起 async 流式工具模型请求"
            ));
        }
        let request =
            build_provider_request(&req, &model, MAX_TOKENS_MAIN, &functions, tool_choice);
        let provider = self.build_provider_dispatch(timeout_ms)?;

        let mut text = String::new();
        let mut reasoning_content = String::new();
        let mut reasoning_signature: Option<String> = None;
        let mut usage = TokenUsageData::default();
        let mut tool_calls: std::collections::BTreeMap<String, (String, String)> =
            std::collections::BTreeMap::new();
        let mut tool_call_order: Vec<String> = Vec::new();

        let mut stream = provider.stream(request).await.map_err(map_llm_error)?;
        while let Some(event) = stream.next().await {
            match event.map_err(map_llm_error)? {
                ProviderStreamEvent::ReasoningDelta(delta) => {
                    if !delta.is_empty() {
                        reasoning_content.push_str(&delta);
                        let _ = chunk_tx.send(ModelStreamChunk {
                            content: String::new(),
                            reasoning_content: delta,
                            usage: None,
                        });
                    }
                }
                ProviderStreamEvent::ReasoningSignatureDelta(signature) => {
                    if !signature.trim().is_empty() {
                        reasoning_signature = Some(signature);
                    }
                }
                ProviderStreamEvent::TextDelta(delta) => {
                    if !delta.is_empty() {
                        text.push_str(&delta);
                        let _ = chunk_tx.send(ModelStreamChunk {
                            content: delta,
                            reasoning_content: String::new(),
                            usage: None,
                        });
                    }
                }
                ProviderStreamEvent::ToolCallStart(call) => {
                    let args = if call.arguments.is_null() || call.arguments == json!({}) {
                        String::new()
                    } else {
                        call.arguments.to_string()
                    };
                    tool_call_order.push(call.id.clone());
                    tool_calls.insert(call.id.clone(), (call.name, args));
                }
                ProviderStreamEvent::ToolCallDelta {
                    call_id,
                    partial_json,
                } => {
                    let actual_id = if tool_calls.contains_key(&call_id) {
                        call_id
                    } else if let Ok(idx) = call_id.parse::<usize>() {
                        tool_call_order.get(idx).cloned().unwrap_or_default()
                    } else {
                        call_id
                    };
                    let entry = tool_calls
                        .entry(actual_id)
                        .or_insert_with(|| (String::new(), String::new()));
                    // 某些 provider 会在 ToolCallStart 里直接给出完整 arguments，
                    // 也可能在 delta 中再发送一遍；这里尽量避免重复拼接导致 JSON 无法解析。
                    if entry.1.trim().is_empty() || !looks_like_complete_json(&entry.1) {
                        entry.1.push_str(&partial_json);
                    }
                }
                ProviderStreamEvent::Usage(stream_usage) => {
                    merge_stream_usage(&mut usage, stream_usage);
                    let _ = chunk_tx.send(ModelStreamChunk {
                        content: String::new(),
                        reasoning_content: String::new(),
                        usage: Some(usage.clone()),
                    });
                }
                ProviderStreamEvent::Error(message) => return Err(anyhow!(message)),
                ProviderStreamEvent::MessageStart
                | ProviderStreamEvent::ToolCallEnd { .. }
                | ProviderStreamEvent::MessageEnd => {}
            }
        }

        let mut tool_calls_vec = Vec::new();
        for (id, (name, raw_args)) in tool_calls.into_iter() {
            if name.trim().is_empty() {
                continue;
            }
            let raw_args_preview = raw_args
                .char_indices()
                .take_while(|(i, _)| *i < 256)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .map(|end| &raw_args[..end])
                .unwrap_or("");
            tracing::info!(
                tool_call_id = %id,
                tool_name = %name,
                raw_args_len = raw_args.len(),
                %raw_args_preview,
                "解析 tool call arguments"
            );
            let arguments = parse_tool_arguments_or_error(&name, &id, &raw_args);
            tool_calls_vec.push(ToolCall {
                id,
                name,
                arguments,
            });
        }

        if text.trim().is_empty()
            && reasoning_content.trim().is_empty()
            && tool_calls_vec.is_empty()
        {
            return Err(anyhow!("async 流式响应缺少文本、思考内容和工具调用"));
        }

        Ok(ModelFunctionResponse {
            text: text.trim().to_string(),
            reasoning_content: reasoning_content.trim().to_string(),
            reasoning_signature: reasoning_signature.filter(|value| !value.trim().is_empty()),
            usage: usage.into(),
            tool_calls: tool_calls_vec,
        })
    }

    pub fn complete_with_functions_with_tool_choice(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
        tool_choice: Option<ToolChoice>,
    ) -> Result<ModelFunctionResponse> {
        if self.protocol() == ProviderProtocol::Anthropic {
            return self.complete_with_functions_anthropic_with_tool_choice(
                req,
                functions,
                tool_choice,
            );
        }

        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起工具模型请求"));
        }
        let provider = self.build_openai_variant_provider(timeout_ms)?;
        let request = build_provider_request(req, model, MAX_TOKENS_MAIN, functions, tool_choice);
        let response = self.block_on_llm(provider.complete(request))?;
        convert_provider_response_to_function_response(response)
    }

    pub fn complete_with_functions_stream_with_tool_choice(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
        tool_choice: Option<ToolChoice>,
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        if use_stream_mode() {
            match self.complete_with_functions_stream_impl_with_tool_choice(
                req,
                functions,
                tool_choice.clone(),
                on_delta,
            ) {
                Ok(resp) => Ok(resp),
                Err(_) => {
                    // 流式失败，回退到非流式，一次性推送 reasoning + content
                    let resp =
                        self.complete_with_functions_with_tool_choice(req, functions, tool_choice)?;
                    if !resp.reasoning_content.is_empty() {
                        on_delta(&ModelStreamChunk {
                            content: String::new(),
                            reasoning_content: resp.reasoning_content.clone(),
                            usage: None,
                        });
                    }
                    if !resp.text.is_empty() {
                        on_delta(&ModelStreamChunk {
                            content: resp.text.clone(),
                            reasoning_content: String::new(),
                            usage: None,
                        });
                    }
                    Ok(resp)
                }
            }
        } else {
            let resp =
                self.complete_with_functions_with_tool_choice(req, functions, tool_choice)?;
            if !resp.reasoning_content.is_empty() {
                on_delta(&ModelStreamChunk {
                    content: String::new(),
                    reasoning_content: resp.reasoning_content.clone(),
                    usage: None,
                });
            }
            if !resp.text.is_empty() {
                on_delta(&ModelStreamChunk {
                    content: resp.text.clone(),
                    reasoning_content: String::new(),
                    usage: None,
                });
            }
            Ok(resp)
        }
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

    fn complete_stream(
        &self,
        req: &ModelRequest,
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelResponse> {
        self.complete_stream_with_callback(req, on_delta)
    }

    fn complete(&self, req: &ModelRequest) -> Result<ModelResponse> {
        if self.protocol() == ProviderProtocol::Anthropic {
            return self.complete_anthropic(req);
        }

        let timeout_ms = parse_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起模型请求"));
        }
        let provider = self.build_openai_variant_provider(timeout_ms)?;
        let request = build_provider_request(req, model, MAX_TOKENS_MAIN, &[], None);
        let response = self.block_on_llm(provider.complete(request))?;
        Ok(ModelResponse {
            text: collect_provider_text(&response).trim().to_string(),
            reasoning_content: response.reasoning_content.unwrap_or_default(),
            reasoning_signature: None,
            usage: response.usage.unwrap_or_default().into(),
            tool_calls: Vec::new(),
        })
    }

    fn complete_with_functions(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
    ) -> Result<ModelFunctionResponse> {
        SingleProviderClient::complete_with_functions_with_tool_choice(self, req, functions, None)
    }

    fn complete_with_functions_stream(
        &self,
        req: &ModelRequest,
        functions: &[ToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        SingleProviderClient::complete_with_functions_stream_with_tool_choice(
            self, req, functions, None, on_delta,
        )
    }
}

/// 主模型请求最大输出 token 数。
///
/// 规划/执行/响应等主模型请求统一使用此值。
/// thinking 模式下 thinking tokens 与 output tokens 共享此预算，
/// 因此 32k 确保 thinking 消耗 15-20k 后仍有 12-17k 给实际输出。
/// 此值从 tiangong-core 到 tiangong-llm 再到 provider 逐层透传，不做任何修改。
const MAX_TOKENS_MAIN: u32 = 32_768;

/// 轻量级任务（标题生成、分类判断等）最大输出 token 数。
/// 任务输出通常不超过几十个字，200 足够且能有效控制成本。
const MAX_TOKENS_LITE: u32 = 200;

fn build_anthropic_provider_from_config(
    cfg: &ModelProviderConfig,
    timeout_ms: u64,
    on_retry: Option<OnRetryCallback>,
) -> Result<AnthropicProvider> {
    let token = cfg.api_auth_token.trim();
    if token.is_empty() {
        return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起 Anthropic 请求"));
    }

    let mut config = AnthropicConfig::new(token.to_string());
    config.base_url = Some(cfg.api_base_url.clone());
    config.timeout = Duration::from_millis(timeout_ms);
    config.max_retries = MAX_RETRIES;
    config.retry_notifier = on_retry;
    AnthropicProvider::from_config(config).map_err(map_llm_error)
}

fn build_openai_provider_from_config(
    cfg: &ModelProviderConfig,
    timeout_ms: u64,
    on_retry: Option<OnRetryCallback>,
) -> Result<OpenAiChatCompletionsProvider> {
    let token = cfg.api_auth_token.trim();
    if token.is_empty() {
        return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起 OpenAI 兼容请求"));
    }
    let mut config = OpenAiChatConfig::new(token.to_string(), cfg.api_base_url.clone());
    config.timeout = Duration::from_millis(timeout_ms);
    config.max_retries = MAX_RETRIES;
    config.retry_notifier = on_retry;
    Ok(OpenAiChatCompletionsProvider::new(config))
}

fn build_openai_responses_provider_from_config(
    cfg: &ModelProviderConfig,
    timeout_ms: u64,
    on_retry: Option<OnRetryCallback>,
) -> Result<OpenAiResponsesProvider> {
    let token = cfg.api_auth_token.trim();
    if token.is_empty() {
        return Err(anyhow!(
            "API_AUTH_TOKEN 不能为空，无法发起 OpenAI Responses 请求"
        ));
    }
    let mut config = OpenAiResponsesConfig::new(token.to_string(), cfg.api_base_url.clone());
    config.timeout = Duration::from_millis(timeout_ms);
    config.max_retries = MAX_RETRIES;
    config.retry_notifier = on_retry;
    Ok(OpenAiResponsesProvider::new(config))
}

fn build_deepseek_provider_from_config(
    cfg: &ModelProviderConfig,
    timeout_ms: u64,
    on_retry: Option<OnRetryCallback>,
) -> Result<DeepSeekProvider> {
    let token = cfg.api_auth_token.trim();
    if token.is_empty() {
        return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起 DeepSeek 请求"));
    }
    let mut config = DeepSeekConfig::new(token.to_string());
    config.base_url = if cfg.api_base_url.trim().is_empty() {
        None
    } else {
        Some(cfg.api_base_url.clone())
    };
    config.timeout = Duration::from_millis(timeout_ms);
    config.max_retries = MAX_RETRIES;
    config.retry_notifier = on_retry;
    DeepSeekProvider::from_config(config).map_err(|err| anyhow!("{err}"))
}

fn build_provider_request(
    req: &ModelRequest,
    model: &str,
    max_tokens: u32,
    functions: &[ToolSpec],
    tool_choice: Option<ToolChoice>,
) -> ProviderRequest {
    let (system, messages) = build_provider_messages(req);
    let thinking = req.thinking.clone();
    ProviderRequest {
        model: model.to_string(),
        system: (!system.trim().is_empty()).then_some(system),
        messages,
        tools: functions.to_vec(),
        tool_choice: tool_choice.or_else(|| (!functions.is_empty()).then_some(LlmToolChoice::Auto)),
        max_tokens,
        temperature: configured_temperature_f32(),
        top_p: None,
        stop_sequences: Vec::new(),
        metadata: None,
        thinking,
        reasoning_effort: req.reasoning_effort,
        thinking_disabled: req.thinking_disabled,
    }
}

const SYSTEM_IDENTITY: &str = "你是天工智能助手，一个功能丰富的个人 AI 中枢。你可以回答问题、处理文件、执行命令、生成多媒体内容，也可以通过工具和技能完成各种复杂任务。";

const SYSTEM_RULES: &str = "规则：\
1. 对话时自然友好，回复内容完整有用。闲聊和问候时正常交流，简单介绍自己的能力。\
2. 需要文件操作、代码搜索、命令执行等实际操作时，调用对应的工具。\
3. 每次工具调用后会收到执行结果，根据结果决定下一步：继续调用工具或给出最终回复。\
4. 执行工具任务时语言简洁高效，不要说\"让我查看\"之类的过渡语，直接给出结果。\
5. 不要在回复中包含工具调用的原始痕迹（如 ok=、exit_code= 等元数据）。\
6. 回复使用 Markdown 格式：代码和命令用代码块包裹，使用标题、列表等结构化排版。\
7. 工具调用失败时必须如实告知用户失败原因，绝对不能虚构成功结果。\
8. 如果已安装的 Skill 能处理用户请求，优先通过 run_command 调用 Skill 脚本。\
9. 耗时较长的命令使用 spawn_task 在后台执行。\
10. 多个可并行的耗时任务使用 spawn+join 模式。";

fn build_provider_messages(req: &ModelRequest) -> (String, Vec<ChatMessage>) {
    let mut messages = Vec::new();
    let mut system_texts = vec![
        SYSTEM_IDENTITY.to_string(),
        SYSTEM_RULES.to_string(),
        format!("当前会话：{}", req.session_title),
        format!("当前工作目录：{}", current_working_directory_text()),
        format!("允许文件操作目录：{}", allowed_file_roots_text()),
    ];
    for msg in &req.context {
        if msg.role == MessageRole::System {
            let text = msg.text_content().trim().to_string();
            if !text.is_empty() {
                system_texts.clear();
                system_texts.push(text);
            }
            continue;
        }
        if let Some(message) = provider_message_from_session(msg, req.include_media) {
            messages.push(message);
        }
    }

    // 跳过消息列表开头的非 User 消息（summary_up_to 截断可能导致以 Assistant/Tool 开头）
    if let Some(idx) = messages.iter().position(|m| m.role == LlmMessageRole::User)
        && idx > 0
    {
        tracing::debug!(
            skipped = idx,
            "跳过消息列表开头的非 User 消息（可能是 summary 截断导致）"
        );
        messages = messages.split_off(idx);
    }

    if !req.user_input.is_empty() {
        messages.push(ChatMessage::text(
            LlmMessageRole::User,
            req.user_input.clone(),
        ));
    }

    let mut messages = sanitize_provider_messages(messages);
    if messages.is_empty() {
        let fallback = if req.user_input.trim().is_empty() {
            "请继续处理当前任务。".to_string()
        } else {
            req.user_input.trim().to_string()
        };
        messages.push(ChatMessage::text(LlmMessageRole::User, fallback));
    }

    (system_texts.join("\n"), messages)
}

fn provider_message_from_session(msg: &Message, include_media: bool) -> Option<ChatMessage> {
    let role = match msg.role {
        MessageRole::User => LlmMessageRole::User,
        MessageRole::Assistant => LlmMessageRole::Assistant,
        MessageRole::Tool => LlmMessageRole::Tool,
        MessageRole::System => return None,
    };

    if msg.role == MessageRole::Tool {
        let text = msg.text_content();
        let Some(tool_call_id) = msg.tool_call_id.as_ref() else {
            if text.trim().is_empty() {
                return None;
            }
            let tool_name = msg.tool_name.as_deref().unwrap_or("runtime_context");
            return Some(ChatMessage::text(
                LlmMessageRole::User,
                format!(
                    "<tool-context name=\"{tool_name}\">\n{}\n</tool-context>",
                    text.trim()
                ),
            ));
        };
        return Some(ChatMessage::new(
            role,
            vec![LlmMessageContent::ToolResult(LlmToolResult {
                tool_call_id: tool_call_id.clone(),
                content: LlmToolResultContent::Text(text),
                is_error: msg.tool_result_is_error,
            })],
        ));
    }

    let mut content = Vec::new();
    if !msg.reasoning_content.trim().is_empty() {
        content.push(LlmMessageContent::Thinking(LlmThinkingContent {
            thinking: msg.reasoning_content.trim().to_string(),
            signature: msg.reasoning_signature.clone(),
        }));
    }

    // 遍历 content blocks
    for block in &msg.content {
        match block {
            ContentBlock::Text { text: s } => {
                if !s.trim().is_empty() {
                    content.push(LlmMessageContent::Text(s.trim().to_string()));
                }
            }
            ContentBlock::Media {
                kind: MediaKind::Image,
                url,
                ..
            } if include_media && msg.role == MessageRole::User => {
                if let Some(img_content) = image_content_from_reference(url) {
                    content.push(LlmMessageContent::Image(img_content));
                }
            }
            // 文件类附件（PDF/Office）不内联注入主请求。
            // 统一交给下方的 attachment_notice 提示 + system prompt 中的
            // 「文档附件解析规则」引导 agent 用本地脚本解析（issue #149）。
            ContentBlock::Media {
                kind: MediaKind::File,
                ..
            } => {}
            ContentBlock::Media { .. } => {}
        }
    }

    if include_media && msg.role == MessageRole::User {
        let image_count = content
            .iter()
            .filter(|c| matches!(c, LlmMessageContent::Image(_)))
            .count();
        if image_count > 0 {
            // 插入图片数量提示（在图片之前）
            let notice = LlmMessageContent::Text(format!(
                "本条用户消息包含 {image_count} 张图片附件，图片内容已随消息提供，请直接基于附件分析。"
            ));
            // 找到第一个 Image 的位置，在其前面插入
            let pos = content
                .iter()
                .position(|c| matches!(c, LlmMessageContent::Image(_)))
                .unwrap_or(content.len());
            content.insert(pos, notice);
        }
    }
    // 文件附件（PDF/Office）统一走 attachment_notice 提示，不内联进请求。
    // 无论 include_media 与否，只要用户消息含文件附件就注入提示，
    // 由 system prompt 中的「文档附件解析规则」引导 agent 本地解析。
    // 对于图片类附件：当 include_media=false 时（chat 模型非多模态）也走提示，
    // 引导调用 analyze_attachment 工具。
    let needs_attachment_notice = msg.role == MessageRole::User
        && (msg.has_file_media() || (!include_media && msg.has_media()));
    if needs_attachment_notice {
        content.push(LlmMessageContent::Text(attachment_notice_text(msg)));
    }

    content.extend(msg.tool_calls.iter().map(|call| {
        LlmMessageContent::ToolCall(LlmToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
    }));
    if content.is_empty() {
        return None;
    }

    Some(ChatMessage::new(role, content))
}

fn attachment_notice_text(msg: &Message) -> String {
    let mut media_index = 0usize;
    let mut has_file_attachment = false;
    let items: Vec<String> = msg
        .content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Media {
                kind,
                url,
                mime_type,
                title,
            } = block
            {
                if matches!(kind, MediaKind::File) {
                    has_file_attachment = true;
                }
                let title = title.as_deref().unwrap_or("未命名附件");
                let mime = mime_type.as_deref().unwrap_or("unknown");
                let idx = media_index;
                media_index += 1;
                // 归档成功的附件 url 是本地路径，可直接引用。
                // 归档失败的附件 url 仍是 data URL（可能长达数 MB），
                // 绝不能原样塞进提示文本，否则会撑爆上下文窗口。
                let path_field = if url.trim_start().starts_with("data:") {
                    "<归档失败，仅有内联 data URL；请告知用户重新上传>".to_string()
                } else {
                    url.clone()
                };
                Some(format!(
                    "- index={idx} kind={kind:?} title={title} mime_type={mime} path={path_field}",
                ))
            } else {
                None
            }
        })
        .collect();
    let items = items.join("\n");
    if has_file_attachment {
        // 文件类附件（PDF/Office）统一走本地脚本解析（issue #149）。
        // path 字段为本地归档路径（~/.tiangong/media/files/...），
        // agent 据此用 run_command 读取并解析，具体方法见 system prompt 中的
        // 「文档附件解析规则」段。
        format!(
            "本条用户消息包含文件附件，文件内容不会直接发送给模型。请使用 run_command 工具按「文档附件解析规则」读取上述 path 路径的文件并解析（path 为本地归档路径，可直接读取）。\n{}",
            items
        )
    } else {
        format!(
            "本条用户消息包含附件，但主模型请求不会直接携带附件内容。需要查看附件内容时，请调用 analyze_attachment 工具，必须使用本提示中的 message_id={}（不要使用其他消息的 ID），并指定附件 index。\n{}",
            msg.id, items
        )
    }
}

fn image_content_from_reference(value: &str) -> Option<tiangong_llm::message::ImageContent> {
    let trimmed = value.trim();
    if trimmed.starts_with("data:image/")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
    {
        return Some(tiangong_llm::message::ImageContent {
            mime_type: image_mime_from_reference(trimmed)
                .unwrap_or_else(|| "image/png".to_string()),
            data: trimmed.to_string(),
        });
    }

    let bytes = std::fs::read(Path::new(trimmed)).ok()?;
    let mime_type = image_mime_from_reference(trimmed).unwrap_or_else(|| "image/png".to_string());
    let data = format!(
        "data:{mime_type};base64,{}",
        general_purpose::STANDARD.encode(bytes)
    );
    Some(tiangong_llm::message::ImageContent { mime_type, data })
}

fn image_mime_from_reference(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.starts_with("data:image/") {
        return lower
            .split_once(';')
            .map(|(mime, _)| mime.trim_start_matches("data:").to_string());
    }
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg".to_string())
    } else if lower.ends_with(".webp") {
        Some("image/webp".to_string())
    } else if lower.ends_with(".gif") {
        Some("image/gif".to_string())
    } else if lower.ends_with(".png") {
        Some("image/png".to_string())
    } else {
        None
    }
}

fn sanitize_provider_messages(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut sanitized: Vec<ChatMessage> = Vec::new();
    let mut seen_user = false;
    let mut deferred_tool_contexts: Vec<ChatMessage> = Vec::new();
    let mut pending_tool_results = 0usize;

    for message in messages {
        if message.role == LlmMessageRole::System || is_empty_provider_message(&message) {
            continue;
        }
        if !seen_user {
            if message.role != LlmMessageRole::User {
                continue;
            }
            seen_user = true;
        }

        if pending_tool_results > 0 {
            if message.role == LlmMessageRole::User && is_internal_tool_context_message(&message) {
                deferred_tool_contexts.push(message);
                continue;
            }
            if message.role == LlmMessageRole::Tool {
                pending_tool_results = pending_tool_results
                    .saturating_sub(provider_message_tool_result_count(&message));
            } else {
                flush_deferred_messages(&mut sanitized, &mut deferred_tool_contexts);
                pending_tool_results = 0;
            }
        }

        if let Some(last) = sanitized.last_mut()
            && last.role == message.role
        {
            merge_provider_message_content(last, message);
        } else {
            pending_tool_results =
                pending_tool_results.max(provider_message_tool_call_count(&message));
            sanitized.push(message);
            continue;
        }
        pending_tool_results =
            pending_tool_results.max(provider_message_tool_call_count(sanitized.last().unwrap()));
    }

    flush_deferred_messages(&mut sanitized, &mut deferred_tool_contexts);

    // 修复上下文截断导致的 tool_call/tool_result 配对不一致
    sanitize_tool_call_pairing(&mut sanitized);

    sanitized
}

/// 确保消息序列中 tool_call 与 tool_result 一一配对。
///
/// 上下文压缩或截断可能在不安全的位置切断消息序列，导致：
/// - 头部出现孤立的 tool result（对应的 assistant tool_call 被截掉了）
/// - 尾部的 assistant 消息包含 tool_call 但没有后续的 tool result
///
/// Anthropic 等提供商会校验配对关系，不一致时直接拒绝请求。
fn sanitize_tool_call_pairing(messages: &mut Vec<ChatMessage>) {
    if messages.is_empty() {
        return;
    }

    // 第一遍：收集所有 tool_call id
    let mut valid_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in messages.iter() {
        for content in &msg.content {
            if let LlmMessageContent::ToolCall(tc) = content {
                valid_call_ids.insert(tc.id.clone());
            }
        }
    }

    // 第二遍：移除没有对应 tool_call 的 tool result（可能是截断残留）
    for msg in messages.iter_mut() {
        if msg.role != LlmMessageRole::Tool {
            continue;
        }
        msg.content.retain(|content| {
            if let LlmMessageContent::ToolResult(tr) = content {
                valid_call_ids.contains(&tr.tool_call_id)
            } else {
                true
            }
        });
    }

    // 移除内容被清空的 tool 消息
    messages.retain(|msg| {
        if msg.role == LlmMessageRole::Tool {
            !msg.content.is_empty()
        } else {
            true
        }
    });

    // 第三遍：处理尾部 assistant 消息中有 tool_call 但无后续 tool_result 的情况
    // 收集所有 tool_result 的 tool_call_id
    let mut result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in messages.iter() {
        for content in &msg.content {
            if let LlmMessageContent::ToolResult(tr) = content {
                result_ids.insert(tr.tool_call_id.clone());
            }
        }
    }

    // 从尾部 assistant 消息中移除没有对应 tool_result 的 tool_call
    // 如果 assistant 消息只剩下 tool_call（没有文本），则整条移除
    let mut i = messages.len();
    while i > 0 {
        i -= 1;
        let msg = &mut messages[i];
        if msg.role != LlmMessageRole::Assistant {
            break;
        }
        let had_tool_calls = msg
            .content
            .iter()
            .any(|c| matches!(c, LlmMessageContent::ToolCall(_)));
        if !had_tool_calls {
            break;
        }

        msg.content.retain(|content| {
            if let LlmMessageContent::ToolCall(tc) = content {
                result_ids.contains(&tc.id)
            } else {
                true
            }
        });

        // 如果 assistant 消息被清空，移除整条
        if msg.content.is_empty() {
            messages.remove(i);
        } else if !msg
            .content
            .iter()
            .any(|c| matches!(c, LlmMessageContent::ToolCall(_)))
        {
            // 不再有 tool_call 了，不需要继续向前处理
            break;
        }
    }
}

fn provider_message_tool_call_count(message: &ChatMessage) -> usize {
    message
        .content
        .iter()
        .filter(|content| matches!(content, LlmMessageContent::ToolCall(_)))
        .count()
}

fn provider_message_tool_result_count(message: &ChatMessage) -> usize {
    message
        .content
        .iter()
        .filter(|content| matches!(content, LlmMessageContent::ToolResult(_)))
        .count()
}

fn is_internal_tool_context_message(message: &ChatMessage) -> bool {
    message.role == LlmMessageRole::User
        && message.content.iter().any(|content| {
            matches!(
                content,
                LlmMessageContent::Text(text) if text.trim_start().starts_with("<tool-context")
            )
        })
}

fn flush_deferred_messages(sanitized: &mut Vec<ChatMessage>, deferred: &mut Vec<ChatMessage>) {
    for message in deferred.drain(..) {
        if let Some(last) = sanitized.last_mut()
            && last.role == message.role
        {
            merge_provider_message_content(last, message);
            continue;
        }
        sanitized.push(message);
    }
}

fn is_empty_provider_message(message: &ChatMessage) -> bool {
    message.content.iter().all(|content| match content {
        LlmMessageContent::Text(text) => text.trim().is_empty(),
        _ => false,
    })
}

fn merge_provider_message_content(target: &mut ChatMessage, source: ChatMessage) {
    for content in source.content {
        match (target.content.last_mut(), content) {
            (Some(LlmMessageContent::Text(current)), LlmMessageContent::Text(next)) => {
                if !next.trim().is_empty() {
                    if !current.trim().is_empty() {
                        current.push_str("\n\n");
                    }
                    current.push_str(next.trim());
                }
            }
            (_, content) => target.content.push(content),
        }
    }
}

fn convert_provider_response_to_function_response(
    response: ProviderResponse,
) -> Result<ModelFunctionResponse> {
    let text = collect_provider_text(&response);
    let reasoning_content = response.reasoning_content.clone().unwrap_or_default();
    let tool_calls = response
        .assistant_message
        .content
        .iter()
        .filter_map(|content| match content {
            LlmMessageContent::ToolCall(LlmToolCall {
                id,
                name,
                arguments,
            }) if !name.is_empty() => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    if text.trim().is_empty() && reasoning_content.trim().is_empty() && tool_calls.is_empty() {
        return Err(anyhow!(
            "Anthropic 响应缺少文本和工具调用（{}）",
            empty_provider_response_diagnostic(&response)
        ));
    }

    Ok(ModelFunctionResponse {
        text: text.trim().to_string(),
        reasoning_content,
        reasoning_signature: collect_provider_reasoning_signature(&response),
        usage: response.usage.unwrap_or_default().into(),
        tool_calls,
    })
}

fn empty_provider_response_diagnostic(response: &ProviderResponse) -> String {
    let mut parts = Vec::new();
    if let Some(model) = response.model.as_deref().filter(|model| !model.is_empty()) {
        parts.push(format!("model={model}"));
    }
    if let Some(id) = response.id.as_deref().filter(|id| !id.is_empty()) {
        parts.push(format!("id={id}"));
    }
    if let Some(stop_reason) = &response.stop_reason {
        parts.push(format!("stop_reason={}", display_stop_reason(stop_reason)));
    }
    parts.push(format!(
        "content_blocks={}",
        response.assistant_message.content.len()
    ));
    if let Some(raw) = response.raw.as_ref()
        && let Some(raw_summary) = summarize_provider_raw_response(raw)
    {
        parts.push(raw_summary);
    }
    parts.join(", ")
}

fn display_stop_reason(reason: &StopReason) -> String {
    match reason {
        StopReason::EndTurn => "end_turn".to_string(),
        StopReason::ToolUse => "tool_use".to_string(),
        StopReason::MaxTokens => "max_tokens".to_string(),
        StopReason::StopSequence => "stop_sequence".to_string(),
        StopReason::Other(value) => value.clone(),
    }
}

fn summarize_provider_raw_response(raw: &Value) -> Option<String> {
    let content = raw.get("content")?.as_array()?;
    if content.is_empty() {
        return Some("raw_content=[]".to_string());
    }
    let blocks = content
        .iter()
        .take(8)
        .enumerate()
        .map(|(index, block)| {
            let block_type = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let keys = block
                .as_object()
                .map(|object| {
                    object
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .unwrap_or_default();
            if keys.is_empty() {
                format!("{index}:{block_type}")
            } else {
                format!("{index}:{block_type}[{keys}]")
            }
        })
        .collect::<Vec<_>>()
        .join(";");
    Some(format!("raw_content={blocks}"))
}

fn parse_tool_arguments_or_error(tool_name: &str, call_id: &str, raw_args: &str) -> Value {
    if raw_args.trim().is_empty() {
        return json!({});
    }

    serde_json::from_str(raw_args).unwrap_or_else(|err| {
        let raw_preview: String = raw_args.chars().take(512).collect();
        json!({
            "__parse_error": format!(
                "工具参数 JSON 无效：tool={tool_name} id={call_id} error={err}。\
        长内容写入请分段调用 write_file，第一次 append=false，后续 append=true。"
            ),
            "__raw_args_preview": raw_preview,
        })
    })
}

fn collect_provider_text(response: &ProviderResponse) -> String {
    response
        .assistant_message
        .content
        .iter()
        .filter_map(|content| match content {
            LlmMessageContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn collect_provider_reasoning_signature(response: &ProviderResponse) -> Option<String> {
    response
        .assistant_message
        .content
        .iter()
        .find_map(|content| {
            if let LlmMessageContent::Thinking(thinking) = content {
                thinking
                    .signature
                    .as_ref()
                    .filter(|signature| !signature.trim().is_empty())
                    .cloned()
            } else {
                None
            }
        })
}

async fn consume_provider_stream_events_async(
    mut stream: ProviderStream,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ModelFunctionResponse> {
    let mut text = String::new();
    let mut reasoning_content = String::new();
    let mut reasoning_signature: Option<String> = None;
    let mut usage = TokenUsageData::default();
    let mut tool_calls: std::collections::BTreeMap<String, (String, String)> =
        std::collections::BTreeMap::new();
    let mut tool_call_order: Vec<String> = Vec::new();

    while let Some(event) = stream.next().await {
        match event.map_err(map_llm_error)? {
            ProviderStreamEvent::ReasoningDelta(delta) => {
                if !delta.is_empty() {
                    reasoning_content.push_str(&delta);
                    on_delta(&ModelStreamChunk {
                        content: String::new(),
                        reasoning_content: delta.clone(),
                        usage: None,
                    });
                }
            }
            ProviderStreamEvent::ReasoningSignatureDelta(signature) => {
                if !signature.trim().is_empty() {
                    reasoning_signature = Some(signature);
                }
            }
            ProviderStreamEvent::TextDelta(delta) => {
                if !delta.is_empty() {
                    text.push_str(&delta);
                    on_delta(&ModelStreamChunk {
                        content: delta,
                        reasoning_content: String::new(),
                        usage: None,
                    });
                }
            }
            ProviderStreamEvent::ToolCallStart(call) => {
                let args = if call.arguments.is_null() || call.arguments == json!({}) {
                    String::new()
                } else {
                    call.arguments.to_string()
                };
                tool_call_order.push(call.id.clone());
                tool_calls.insert(call.id.clone(), (call.name, args));
            }
            ProviderStreamEvent::ToolCallDelta {
                call_id,
                partial_json,
            } => {
                let actual_id = if tool_calls.contains_key(&call_id) {
                    call_id
                } else if let Ok(idx) = call_id.parse::<usize>() {
                    tool_call_order.get(idx).cloned().unwrap_or_default()
                } else {
                    call_id
                };
                let entry = tool_calls
                    .entry(actual_id)
                    .or_insert_with(|| (String::new(), String::new()));
                entry.1.push_str(&partial_json);
            }
            ProviderStreamEvent::Usage(stream_usage) => {
                merge_stream_usage(&mut usage, stream_usage)
            }
            ProviderStreamEvent::Error(message) => return Err(anyhow!(message)),
            ProviderStreamEvent::MessageStart
            | ProviderStreamEvent::ToolCallEnd { .. }
            | ProviderStreamEvent::MessageEnd => {}
        }
    }

    let tool_calls = tool_calls
        .into_iter()
        .filter(|(_, (name, _))| !name.is_empty())
        .map(|(id, (name, raw_args))| ToolCall {
            arguments: parse_tool_arguments_or_error(&name, &id, &raw_args),
            id,
            name,
        })
        .collect::<Vec<_>>();

    if text.trim().is_empty() && reasoning_content.trim().is_empty() && tool_calls.is_empty() {
        return Err(anyhow!("Anthropic 流式响应缺少文本、思考内容和工具调用"));
    }

    Ok(ModelFunctionResponse {
        text: text.trim().to_string(),
        reasoning_content: reasoning_content.trim().to_string(),
        reasoning_signature: reasoning_signature.filter(|value| !value.trim().is_empty()),
        usage: usage.into(),
        tool_calls,
    })
}

enum ProviderDispatch {
    Anthropic(Box<AnthropicProvider>),
    OpenAi(Box<OpenAiChatCompletionsProvider>),
    OpenAiResponses(Box<OpenAiResponsesProvider>),
    DeepSeek(Box<DeepSeekProvider>),
}

impl ProviderDispatch {
    async fn stream(
        self,
        request: ProviderRequest,
    ) -> std::result::Result<ProviderStream, tiangong_llm::error::LlmError> {
        match self {
            ProviderDispatch::Anthropic(provider) => provider.stream(request).await,
            ProviderDispatch::OpenAi(provider) => provider.stream(request).await,
            ProviderDispatch::OpenAiResponses(provider) => provider.stream(request).await,
            ProviderDispatch::DeepSeek(provider) => provider.stream(request).await,
        }
    }
}

fn consume_provider_stream(
    provider: ProviderDispatch,
    request: ProviderRequest,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ModelResponse> {
    let response = block_on_provider_stream(provider, request, on_delta)?;
    Ok(ModelResponse {
        text: response.text,
        reasoning_content: response.reasoning_content,
        reasoning_signature: response.reasoning_signature,
        usage: response.usage,
        tool_calls: response.tool_calls,
    })
}

fn convert_stream_to_function_response(
    provider: ProviderDispatch,
    request: ProviderRequest,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ModelFunctionResponse> {
    block_on_provider_stream(provider, request, on_delta)
}

fn block_on_provider_stream(
    provider: ProviderDispatch,
    request: ProviderRequest,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ModelFunctionResponse> {
    async fn run_stream(
        provider: ProviderDispatch,
        request: ProviderRequest,
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        let stream = provider.stream(request).await.map_err(map_llm_error)?;
        consume_provider_stream_events_async(stream, on_delta).await
    }

    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(run_stream(provider, request, on_delta)))
        }
        Ok(_) => {
            let (response, chunks) = std::thread::scope(|scope| {
                scope
                    .spawn(move || {
                        let runtime = TokioRuntimeBuilder::new_current_thread()
                            .enable_all()
                            .build()
                            .context("初始化异步运行时失败")?;
                        let mut chunks = Vec::new();
                        let response =
                            runtime.block_on(run_stream(provider, request, &mut |chunk| {
                                chunks.push(chunk.clone())
                            }))?;
                        Ok::<_, anyhow::Error>((response, chunks))
                    })
                    .join()
                    .map_err(|_| anyhow!("LLM 流式请求线程 panic"))?
            })?;
            for chunk in chunks {
                on_delta(&chunk);
            }
            Ok(response)
        }
        Err(_) => {
            let runtime = TokioRuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .context("初始化异步运行时失败")?;
            runtime.block_on(run_stream(provider, request, on_delta))
        }
    }
}

fn map_llm_error(error: tiangong_llm::error::LlmError) -> anyhow::Error {
    anyhow!(error.to_string())
}

fn current_working_directory_text() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

fn allowed_file_roots_text() -> String {
    let workspace = current_working_directory_text();
    let temp = std::env::temp_dir().display().to_string();
    format!("{workspace}；{temp}")
}

// ── 重试相关 ──────────────────────────────────────────────

/// 最大重试次数
const MAX_RETRIES: u32 = 3;

/// 清理非流式响应中的 `<think>...</think>` 标签
///
/// 部分模型即使设置了 `thinking.type=disabled`，仍在 content 中输出 `<think>` 标签。
/// 此函数移除所有 `<think>...</think>` 块，返回纯文本内容。
fn strip_think_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(start) = remaining.find("<think>") {
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start..].find("</think>") {
            remaining = &remaining[start + end + 8..];
        } else {
            // 没有闭合标签，丢弃从 <think> 开始的所有内容
            return result;
        }
    }
    result.push_str(remaining);
    result
}

fn configured_temperature_number() -> Option<serde_json::Number> {
    let raw = std::env::var("API_TEMPERATURE").unwrap_or_else(|_| "0.2".to_string());
    let value = raw.trim().parse::<f64>().ok()?;
    if !(0.0..=2.0).contains(&value) {
        return None;
    }
    let rounded = (value * 100.0).round() / 100.0;
    serde_json::Number::from_f64(rounded)
}

fn configured_temperature_f32() -> Option<f32> {
    configured_temperature_number()
        .and_then(|value| value.as_f64())
        .map(|value| value as f32)
}

fn use_stream_mode() -> bool {
    match std::env::var("API_STREAM") {
        Ok(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "0" | "false" | "off" | "no")
        }
        Err(_) => true,
    }
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

fn parse_function_timeout_ms(raw: &str) -> Result<u64> {
    let fallback = parse_timeout_ms(raw)?;
    if let Ok(custom) = std::env::var("API_FUNCTION_TIMEOUT_MS") {
        let parsed = custom
            .trim()
            .parse::<u64>()
            .context("API_FUNCTION_TIMEOUT_MS 解析失败，必须是毫秒数字")?;
        if parsed == 0 {
            return Err(anyhow!("API_FUNCTION_TIMEOUT_MS 必须大于 0"));
        }
        return Ok(parsed);
    }

    // 工具调用阶段默认用更保守的超时，避免长时间卡住导致后续 plan 看似不执行。
    Ok(fallback.min(120_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_defers_internal_tool_context_until_tool_results_complete() {
        let messages = vec![
            ChatMessage::text(LlmMessageRole::User, "开始"),
            ChatMessage::new(
                LlmMessageRole::Assistant,
                vec![
                    LlmMessageContent::ToolCall(LlmToolCall {
                        id: "call_1".to_string(),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": "a"}),
                    }),
                    LlmMessageContent::ToolCall(LlmToolCall {
                        id: "call_2".to_string(),
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": "b"}),
                    }),
                ],
            ),
            ChatMessage::new(
                LlmMessageRole::Tool,
                vec![LlmMessageContent::ToolResult(LlmToolResult {
                    tool_call_id: "call_1".to_string(),
                    content: LlmToolResultContent::Text("a".to_string()),
                    is_error: false,
                })],
            ),
            ChatMessage::text(
                LlmMessageRole::User,
                "<tool-context name=\"read_file\">trace</tool-context>",
            ),
            ChatMessage::new(
                LlmMessageRole::Tool,
                vec![LlmMessageContent::ToolResult(LlmToolResult {
                    tool_call_id: "call_2".to_string(),
                    content: LlmToolResultContent::Text("b".to_string()),
                    is_error: false,
                })],
            ),
        ];

        let sanitized = sanitize_provider_messages(messages);

        assert_eq!(sanitized.len(), 4);
        assert_eq!(sanitized[2].role, LlmMessageRole::Tool);
        assert_eq!(provider_message_tool_result_count(&sanitized[2]), 2);
        assert_eq!(sanitized[3].role, LlmMessageRole::User);
        assert!(is_internal_tool_context_message(&sanitized[3]));
    }

    #[test]
    fn file_attachment_injects_notice_with_path() {
        // 含 PDF 文件附件的 user 消息，必须注入 attachment_notice（含 path），
        // 而不是把文件内联或静默丢弃（issue #149 的核心保证）。
        let msg = test_user_message_with_media(
            "请处理这些附件。",
            MediaKind::File,
            "/Users/test/.tiangong/media/files/abc.pdf",
            Some("application/pdf"),
            Some("测试文档.pdf"),
        );

        // include_media=true（chat 模型多模态）：文件仍走 notice，不内联
        let result = provider_message_from_session(&msg, true).expect("应生成消息");
        let texts: Vec<&str> = result
            .content
            .iter()
            .filter_map(|c| match c {
                LlmMessageContent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let notice = texts
            .iter()
            .find(|t| t.contains("path="))
            .unwrap_or_else(|| panic!("应包含 attachment_notice，实际 texts: {texts:?}"));
        assert!(
            notice.contains("path=/Users/test/.tiangong/media/files/abc.pdf"),
            "notice 应含文件路径，实际：{notice}"
        );
        assert!(
            notice.contains("文件附件"),
            "notice 应说明是文件附件，实际：{notice}"
        );
        // 不应有内联 File content
        assert!(
            !result
                .content
                .iter()
                .any(|c| matches!(c, LlmMessageContent::File(_))),
            "文件不应内联为 File content"
        );
    }

    #[test]
    fn file_attachment_notice_injected_even_when_include_media_false() {
        // include_media=false 时同样应注入 notice
        let msg = test_user_message_with_media(
            "分析这个",
            MediaKind::File,
            "/tmp/doc.docx",
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            None,
        );
        let result = provider_message_from_session(&msg, false).expect("应生成消息");
        assert!(result.content.iter().any(|c| matches!(
            c,
            LlmMessageContent::Text(t) if t.contains("path=/tmp/doc.docx")
        )));
    }

    #[test]
    fn image_attachment_still_inlines_when_multimodal() {
        // 图片附件在 include_media=true 时应内联，不走 notice（回归保护）
        let msg = test_user_message_with_media(
            "看这张图",
            MediaKind::Image,
            "data:image/png;base64,iVBORw0KGgo=",
            Some("image/png"),
            None,
        );
        let result = provider_message_from_session(&msg, true).expect("应生成消息");
        assert!(
            result
                .content
                .iter()
                .any(|c| matches!(c, LlmMessageContent::Image(_))),
            "图片应内联为 Image content"
        );
    }

    /// 构造测试用 user Message（含一个 media block）
    fn test_user_message_with_media(
        text: &str,
        kind: MediaKind,
        url: &str,
        mime_type: Option<&str>,
        title: Option<&str>,
    ) -> Message {
        Message {
            id: "msg_test".to_string(),
            role: MessageRole::User,
            content: vec![
                ContentBlock::text(text),
                ContentBlock::Media {
                    kind,
                    url: url.to_string(),
                    mime_type: mime_type.map(str::to_string),
                    title: title.map(str::to_string),
                },
            ],
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            created_at: String::new(),
            media_migrated: true,
        }
    }

    #[test]
    fn end_to_end_request_contains_rules_and_notice() {
        // 端到端验证：完整 ModelRequest 构建（build_provider_messages）后，
        // system prompt 必须含「文档附件解析规则」，user message 必须含
        // attachment_notice（含文件路径）。
        // 这是离实际 LLM 请求最近的测试，能暴露注入链路的任何断点。
        let user_msg = test_user_message_with_media(
            "请处理这些附件。",
            MediaKind::File,
            "/Users/test/.tiangong/media/files/report.pdf",
            Some("application/pdf"),
            Some("report.pdf"),
        );

        // 构造一条含解析规则的 System 消息（模拟 rebuild_system_prompt 的输出）
        let system_msg = Message {
            id: "sys".to_string(),
            role: MessageRole::System,
            content: vec![ContentBlock::text(
                "你是天工助手。\n## 文档附件解析规则\n源文件已归档在 ~/.tiangong/media/files/。",
            )],
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            created_at: String::new(),
            media_migrated: true,
        };

        let req = ModelRequest {
            session_title: "测试".to_string(),
            user_input: String::new(),
            context: vec![system_msg, user_msg],
            thinking: None,
            reasoning_effort: None,
            thinking_disabled: false,
            include_media: true,
        };

        let (system, messages) = build_provider_messages(&req);

        // system prompt 必须含解析规则（来自 System 消息，不是 fallback 常量）
        assert!(
            system.contains("文档附件解析规则"),
            "system prompt 应含解析规则，实际前200字：{}",
            &system[..system.len().min(200)]
        );
        assert!(
            system.contains("media/files"),
            "system prompt 应含 media/files 路径说明"
        );

        // user message 必须含 attachment_notice（含文件路径）
        let user_content = messages
            .iter()
            .find(|m| m.role == LlmMessageRole::User)
            .expect("应有 user 消息");
        let has_notice = user_content.content.iter().any(|c| match c {
            LlmMessageContent::Text(t) => {
                t.contains("path=/Users/test/.tiangong/media/files/report.pdf")
            }
            _ => false,
        });
        assert!(has_notice, "user message 应含带路径的 attachment_notice");
    }
}
