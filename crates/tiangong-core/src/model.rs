use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::mcp::build_mcp_tools_system_prompt;
use crate::session::{Message, MessageRole};
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
use tiangong_llm::providers::openai::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use tiangong_llm::request::{ProviderRequest, ThinkingConfig as LlmThinkingConfig};
use tiangong_llm::response::ProviderResponse;
use tiangong_llm::stream::{ProviderStream, ProviderStreamEvent};
use tiangong_llm::tool::{
    ToolCall as LlmToolCall, ToolChoice as LlmToolChoice, ToolResult as LlmToolResult,
    ToolResultContent as LlmToolResultContent, ToolSpec,
};
use tiangong_llm::usage::TokenUsageData;
use tokio::runtime::Builder as TokioRuntimeBuilder;

pub use tiangong_llm::ProviderProtocol;
pub use tiangong_llm::tool::ToolChoice as ModelToolChoice;
pub use tiangong_types::TokenUsage;

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub session_title: String,
    pub user_input: String,
    pub context: Vec<Message>,
    /// 已由 PromptAssembler 装配的 system prompt。
    pub assembled_system_prompt: Option<String>,
    pub thinking: Option<ModelThinkingConfig>,
}

#[derive(Debug, Clone)]
pub struct ModelThinkingConfig {
    pub budget_tokens: u32,
}

impl ModelRequest {
    /// 带 assembled_system_prompt 的构造
    pub fn with_assembled_prompt(
        session_title: String,
        user_input: String,
        context: Vec<Message>,
        system_prompt: String,
    ) -> Self {
        Self {
            session_title,
            user_input,
            context,
            assembled_system_prompt: Some(system_prompt),
            thinking: None,
        }
    }

    pub fn with_thinking_budget(mut self, budget_tokens: u32) -> Self {
        self.thinking = Some(ModelThinkingConfig { budget_tokens });
        self
    }
}

#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub text: String,
    pub reasoning_content: String,
    pub reasoning_signature: Option<String>,
    pub usage: TokenUsage,
    pub output_mode: String,
    pub output_chunk_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ModelStreamChunk {
    pub content: String,
    pub reasoning_content: String,
}

#[derive(Debug, Clone)]
pub struct FunctionToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct ModelFunctionCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct ModelFunctionResponse {
    pub text: String,
    pub reasoning_content: String,
    pub reasoning_signature: Option<String>,
    pub usage: TokenUsage,
    pub tool_calls: Vec<ModelFunctionCall>,
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
            });
        }
        if !resp.text.is_empty() {
            on_delta(&ModelStreamChunk {
                content: resp.text.clone(),
                reasoning_content: String::new(),
            });
        }
        Ok(resp)
    }
    fn complete_with_functions(
        &self,
        req: &ModelRequest,
        _functions: &[FunctionToolSpec],
    ) -> Result<ModelFunctionResponse> {
        let resp = self.complete(req)?;
        Ok(ModelFunctionResponse {
            text: resp.text,
            reasoning_content: resp.reasoning_content,
            reasoning_signature: resp.reasoning_signature,
            usage: resp.usage,
            tool_calls: Vec::new(),
        })
    }
    /// 流式函数调用，通过 on_delta 实时回调 thinking chunk
    fn complete_with_functions_stream(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        let resp = self.complete_with_functions(req, functions)?;
        if !resp.reasoning_content.is_empty() {
            on_delta(&ModelStreamChunk {
                content: String::new(),
                reasoning_content: resp.reasoning_content.clone(),
            });
        }
        if !resp.text.is_empty() {
            on_delta(&ModelStreamChunk {
                content: resp.text.clone(),
                reasoning_content: String::new(),
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

    fn protocol(&self) -> ProviderProtocol {
        self.cfg.api_protocol
    }

    fn build_anthropic_provider(&self, timeout_ms: u64) -> Result<AnthropicProvider> {
        build_anthropic_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())
    }

    fn build_provider_dispatch(&self, timeout_ms: u64) -> Result<ProviderDispatch> {
        match self.protocol() {
            ProviderProtocol::Anthropic => Ok(ProviderDispatch::Anthropic(Box::new(
                self.build_anthropic_provider(timeout_ms)?,
            ))),
            ProviderProtocol::OpenAiCompatible => Ok(ProviderDispatch::OpenAi(Box::new(
                build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?,
            ))),
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
        let response = self.complete_with_functions_anthropic(req, &[])?;
        Ok(ModelResponse {
            text: response.text,
            reasoning_content: response.reasoning_content,
            reasoning_signature: response.reasoning_signature,
            usage: response.usage,
            output_mode: "anthropic-non-stream".to_string(),
            output_chunk_count: 1,
        })
    }

    fn complete_with_functions_anthropic(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
    ) -> Result<ModelFunctionResponse> {
        self.complete_with_functions_anthropic_with_tool_choice(req, functions, None)
    }

    fn complete_with_functions_anthropic_with_tool_choice(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        tool_choice: Option<ModelToolChoice>,
    ) -> Result<ModelFunctionResponse> {
        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起 Anthropic 请求"));
        }

        let provider = self.build_anthropic_provider(timeout_ms)?;
        let request =
            build_provider_request(req, model, anthropic_max_tokens(), functions, tool_choice);
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
            max_tokens: Some(200),
            temperature: Some(0.3),
            top_p: None,
            stop_sequences: Vec::new(),
            metadata: None,
            thinking: None,
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
        let request = build_provider_request(req, model, anthropic_max_tokens(), &[], None);
        let provider = self.build_provider_dispatch(timeout_ms)?;
        consume_provider_stream(provider, request, &mut on_delta)
    }

    /// 流式函数调用：实时输出 thinking，同时累积 tool_calls
    pub fn complete_with_functions_stream_impl(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        self.complete_with_functions_stream_impl_with_tool_choice(req, functions, None, on_delta)
    }

    pub fn complete_with_functions_stream_impl_with_tool_choice(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        tool_choice: Option<ModelToolChoice>,
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起流式工具模型请求"));
        }
        let request =
            build_provider_request(req, model, anthropic_max_tokens(), functions, tool_choice);
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
        let provider =
            build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?;
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
            max_tokens: Some(200),
            temperature: Some(0.3),
            top_p: None,
            stop_sequences: Vec::new(),
            metadata: None,
            thinking: None,
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
            max_tokens: Some(200),
            temperature: Some(0.1),
            top_p: None,
            stop_sequences: Vec::new(),
            metadata: None,
            thinking: None,
        };
        if self.protocol() == ProviderProtocol::Anthropic {
            let provider = self.build_anthropic_provider(timeout_ms)?;
            let response = self.block_on_llm(provider.complete(request))?;
            return Ok(strip_think_tags(&collect_provider_text(&response))
                .trim()
                .to_string());
        }
        let provider =
            build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?;
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
        functions: Vec<FunctionToolSpec>,
        chunk_tx: tokio::sync::mpsc::UnboundedSender<ModelStreamChunk>,
    ) -> Result<ModelFunctionResponse> {
        self.stream_function_calls_with_tool_choice(req, functions, None, chunk_tx)
            .await
    }

    pub async fn stream_function_calls_with_tool_choice(
        self,
        req: ModelRequest,
        functions: Vec<FunctionToolSpec>,
        tool_choice: Option<ModelToolChoice>,
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
                    });
                }
                if !response.text.is_empty() {
                    let _ = fallback_tx.send(ModelStreamChunk {
                        content: response.text.clone(),
                        reasoning_content: String::new(),
                    });
                }
                Ok(response)
            }
        }
    }

    async fn stream_function_calls_streaming(
        self,
        req: ModelRequest,
        functions: Vec<FunctionToolSpec>,
        tool_choice: Option<ModelToolChoice>,
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
        let request = build_provider_request(
            &req,
            &model,
            anthropic_max_tokens(),
            &functions,
            tool_choice,
        );
        let provider = self.build_provider_dispatch(timeout_ms)?;

        let mut text = String::new();
        let mut reasoning_content = String::new();
        let mut reasoning_signature: Option<String> = None;
        let mut usage = TokenUsageData::default();
        let mut tool_calls: std::collections::BTreeMap<String, (String, String)> =
            std::collections::BTreeMap::new();

        let mut stream = provider.stream(request).await.map_err(map_llm_error)?;
        while let Some(event) = stream.next().await {
            match event.map_err(map_llm_error)? {
                ProviderStreamEvent::ReasoningDelta(delta) => {
                    if !delta.is_empty() {
                        reasoning_content.push_str(&delta);
                        let _ = chunk_tx.send(ModelStreamChunk {
                            content: String::new(),
                            reasoning_content: delta,
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
                        });
                    }
                }
                ProviderStreamEvent::ToolCallStart(call) => {
                    let args = if call.arguments.is_null() || call.arguments == json!({}) {
                        String::new()
                    } else {
                        call.arguments.to_string()
                    };
                    tool_calls.insert(call.id.clone(), (call.name, args));
                }
                ProviderStreamEvent::ToolCallDelta {
                    call_id,
                    partial_json,
                } => {
                    let entry = tool_calls
                        .entry(call_id)
                        .or_insert_with(|| (String::new(), String::new()));
                    // 某些 provider 会在 ToolCallStart 里直接给出完整 arguments，
                    // 也可能在 delta 中再发送一遍；这里尽量避免重复拼接导致 JSON 无法解析。
                    if entry.1.trim().is_empty() || !looks_like_complete_json(&entry.1) {
                        entry.1.push_str(&partial_json);
                    }
                }
                ProviderStreamEvent::Usage(stream_usage) => usage = stream_usage,
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
            let arguments = parse_tool_arguments_or_error(&name, &id, &raw_args);
            tool_calls_vec.push(ModelFunctionCall {
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
        functions: &[FunctionToolSpec],
        tool_choice: Option<ModelToolChoice>,
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
        let provider =
            build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?;
        let request =
            build_provider_request(req, model, anthropic_max_tokens(), functions, tool_choice);
        let response = self.block_on_llm(provider.complete(request))?;
        convert_provider_response_to_function_response(response)
    }

    pub fn complete_with_functions_stream_with_tool_choice(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        tool_choice: Option<ModelToolChoice>,
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
                        });
                    }
                    if !resp.text.is_empty() {
                        on_delta(&ModelStreamChunk {
                            content: resp.text.clone(),
                            reasoning_content: String::new(),
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
                });
            }
            if !resp.text.is_empty() {
                on_delta(&ModelStreamChunk {
                    content: resp.text.clone(),
                    reasoning_content: String::new(),
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
        let provider =
            build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?;
        let request = build_provider_request(req, model, anthropic_max_tokens(), &[], None);
        let response = self.block_on_llm(provider.complete(request))?;
        Ok(ModelResponse {
            text: collect_provider_text(&response).trim().to_string(),
            reasoning_content: response.reasoning_content.unwrap_or_default(),
            reasoning_signature: None,
            usage: response.usage.unwrap_or_default().into(),
            output_mode: "non-stream".to_string(),
            output_chunk_count: 1,
        })
    }

    fn complete_with_functions(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
    ) -> Result<ModelFunctionResponse> {
        SingleProviderClient::complete_with_functions_with_tool_choice(self, req, functions, None)
    }

    fn complete_with_functions_stream(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        SingleProviderClient::complete_with_functions_stream_with_tool_choice(
            self, req, functions, None, on_delta,
        )
    }
}

fn anthropic_max_tokens() -> u32 {
    configured_max_tokens().map(u32::from).unwrap_or(4096)
}
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
) -> Result<OpenAiCompatibleProvider> {
    let token = cfg.api_auth_token.trim();
    if token.is_empty() {
        return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起 OpenAI 兼容请求"));
    }
    let mut config = OpenAiCompatibleConfig::new(token.to_string(), cfg.api_base_url.clone());
    config.timeout = Duration::from_millis(timeout_ms);
    config.max_retries = MAX_RETRIES;
    config.retry_notifier = on_retry;
    Ok(OpenAiCompatibleProvider::new(config))
}

fn build_provider_request(
    req: &ModelRequest,
    model: &str,
    max_tokens: u32,
    functions: &[FunctionToolSpec],
    tool_choice: Option<ModelToolChoice>,
) -> ProviderRequest {
    let (system, messages) = build_provider_messages(req);
    let thinking = req.thinking.as_ref().map(|thinking| LlmThinkingConfig {
        budget_tokens: thinking.budget_tokens,
    });
    ProviderRequest {
        model: model.to_string(),
        system: (!system.trim().is_empty()).then_some(system),
        messages,
        tools: functions
            .iter()
            .map(|tool| ToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.parameters.clone(),
            })
            .collect(),
        tool_choice: tool_choice.or_else(|| (!functions.is_empty()).then_some(LlmToolChoice::Auto)),
        max_tokens: Some(max_tokens),
        temperature: configured_temperature_f32(),
        top_p: None,
        stop_sequences: Vec::new(),
        metadata: None,
        thinking,
    }
}

fn build_provider_messages(req: &ModelRequest) -> (String, Vec<ChatMessage>) {
    let mut messages = Vec::new();

    let system_texts = if let Some(ref assembled) = req.assembled_system_prompt {
        let texts = vec![assembled.clone()];
        for msg in &req.context {
            if let Some(message) = provider_message_from_session(msg) {
                messages.push(message);
            }
        }
        texts
    } else {
        let mut texts = vec![
            format!("当前会话：{}", req.session_title),
            format!("当前工作目录：{}", current_working_directory_text()),
            format!("允许文件操作目录：{}", allowed_file_roots_text()),
        ];
        if let Some(mcp_tools_prompt) = build_mcp_tools_system_prompt(24) {
            texts.push(mcp_tools_prompt);
        }
        for msg in &req.context {
            if let Some(message) = provider_message_from_session(msg) {
                messages.push(message);
            }
        }
        texts
    };

    if req.assembled_system_prompt.is_none() && !req.user_input.is_empty() {
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

fn provider_message_from_session(msg: &Message) -> Option<ChatMessage> {
    let role = match msg.role {
        MessageRole::User => LlmMessageRole::User,
        MessageRole::Assistant => LlmMessageRole::Assistant,
        MessageRole::Tool => LlmMessageRole::Tool,
        MessageRole::System => return None,
    };

    if msg.role == MessageRole::Tool {
        let Some(tool_call_id) = msg.tool_call_id.as_ref() else {
            let text = msg.content.trim();
            if text.is_empty() {
                return None;
            }
            let tool_name = msg.tool_name.as_deref().unwrap_or("runtime_context");
            return Some(ChatMessage::text(
                LlmMessageRole::User,
                format!("<tool-context name=\"{tool_name}\">\n{text}\n</tool-context>"),
            ));
        };
        return Some(ChatMessage::new(
            role,
            vec![LlmMessageContent::ToolResult(LlmToolResult {
                tool_call_id: tool_call_id.clone(),
                content: LlmToolResultContent::Text(msg.content.clone()),
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
    if !msg.content.trim().is_empty() {
        content.push(LlmMessageContent::Text(msg.content.trim().to_string()));
    }
    if msg.role == MessageRole::User {
        let image_contents = message_image_contents(msg);
        if !image_contents.is_empty() {
            content.push(LlmMessageContent::Text(format!(
                "本条用户消息包含 {} 张图片附件，图片内容已随消息提供，请直接基于附件分析。",
                image_contents.len()
            )));
        }
        content.extend(image_contents);
        let file_contents = message_file_contents(msg);
        if !file_contents.is_empty() {
            content.push(LlmMessageContent::Text(format!(
                "本条用户消息包含 {} 个文件附件，文件内容已以 base64 data URL 随消息提供，请直接基于附件分析。",
                file_contents.len()
            )));
        }
        content.extend(file_contents);
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

fn message_image_contents(msg: &Message) -> Vec<LlmMessageContent> {
    let mut refs = Vec::new();
    for asset in &msg.media {
        if matches!(
            asset.kind,
            tiangong_types::MediaKind::Image | tiangong_types::MediaKind::File
        ) && looks_like_image_reference(&asset.url)
        {
            refs.push(asset.url.clone());
        }
    }
    refs.extend(extract_image_paths_from_text(&msg.content));
    refs.sort();
    refs.dedup();

    refs.into_iter()
        .filter_map(|value| image_content_from_reference(&value))
        .map(LlmMessageContent::Image)
        .collect()
}

fn message_file_contents(msg: &Message) -> Vec<LlmMessageContent> {
    msg.media
        .iter()
        .filter(|asset| matches!(asset.kind, tiangong_types::MediaKind::File))
        .filter(|asset| asset.url.trim_start().starts_with("data:"))
        .filter(|asset| !looks_like_image_reference(&asset.url))
        .map(|asset| {
            LlmMessageContent::File(tiangong_llm::message::FileContent {
                mime_type: asset
                    .mime_type
                    .clone()
                    .or_else(|| mime_from_data_url(&asset.url))
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                data: asset.url.clone(),
                title: asset.title.clone(),
            })
        })
        .collect()
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

fn looks_like_image_reference(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.starts_with("data:image/")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".webp")
        || lower.ends_with(".gif")
}

fn extract_image_paths_from_text(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|part| {
            part.trim_matches(|c: char| {
                matches!(
                    c,
                    '"' | '\'' | '`' | ',' | ';' | ':' | ')' | '(' | '[' | ']'
                )
            })
        })
        .filter(|part| looks_like_image_reference(part))
        .map(ToString::to_string)
        .collect()
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

fn mime_from_data_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    trimmed
        .strip_prefix("data:")
        .and_then(|raw| raw.split(';').next())
        .filter(|mime| !mime.trim().is_empty())
        .map(str::to_string)
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
    sanitized
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
            }) if !name.is_empty() => Some(ModelFunctionCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();

    if text.trim().is_empty() && reasoning_content.trim().is_empty() && tool_calls.is_empty() {
        return Err(anyhow!("Anthropic 响应缺少文本和工具调用"));
    }

    Ok(ModelFunctionResponse {
        text: text.trim().to_string(),
        reasoning_content,
        reasoning_signature: collect_provider_reasoning_signature(&response),
        usage: response.usage.unwrap_or_default().into(),
        tool_calls,
    })
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

    while let Some(event) = stream.next().await {
        match event.map_err(map_llm_error)? {
            ProviderStreamEvent::ReasoningDelta(delta) => {
                if !delta.is_empty() {
                    reasoning_content.push_str(&delta);
                    on_delta(&ModelStreamChunk {
                        content: String::new(),
                        reasoning_content: delta.clone(),
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
                    });
                }
            }
            ProviderStreamEvent::ToolCallStart(call) => {
                let args = if call.arguments.is_null() || call.arguments == json!({}) {
                    String::new()
                } else {
                    call.arguments.to_string()
                };
                tool_calls.insert(call.id.clone(), (call.name, args));
            }
            ProviderStreamEvent::ToolCallDelta {
                call_id,
                partial_json,
            } => {
                let entry = tool_calls
                    .entry(call_id)
                    .or_insert_with(|| (String::new(), String::new()));
                entry.1.push_str(&partial_json);
            }
            ProviderStreamEvent::Usage(stream_usage) => usage = stream_usage,
            ProviderStreamEvent::Error(message) => return Err(anyhow!(message)),
            ProviderStreamEvent::MessageStart
            | ProviderStreamEvent::ToolCallEnd { .. }
            | ProviderStreamEvent::MessageEnd => {}
        }
    }

    let tool_calls = tool_calls
        .into_iter()
        .filter(|(_, (name, _))| !name.is_empty())
        .map(|(id, (name, raw_args))| ModelFunctionCall {
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
    OpenAi(Box<OpenAiCompatibleProvider>),
}

impl ProviderDispatch {
    async fn stream(
        self,
        request: ProviderRequest,
    ) -> std::result::Result<ProviderStream, tiangong_llm::error::LlmError> {
        match self {
            ProviderDispatch::Anthropic(provider) => provider.stream(request).await,
            ProviderDispatch::OpenAi(provider) => provider.stream(request).await,
        }
    }
}

fn consume_provider_stream(
    provider: ProviderDispatch,
    request: ProviderRequest,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ModelResponse> {
    let response = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化异步运行时失败")?
        .block_on(async {
            let stream = provider.stream(request).await.map_err(map_llm_error)?;
            consume_provider_stream_events_async(stream, on_delta).await
        })?;
    Ok(ModelResponse {
        text: response.text,
        reasoning_content: response.reasoning_content,
        reasoning_signature: response.reasoning_signature,
        usage: response.usage,
        output_mode: "stream".to_string(),
        output_chunk_count: 1,
    })
}

fn convert_stream_to_function_response(
    provider: ProviderDispatch,
    request: ProviderRequest,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ModelFunctionResponse> {
    TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化异步运行时失败")?
        .block_on(async {
            let stream = provider.stream(request).await.map_err(map_llm_error)?;
            consume_provider_stream_events_async(stream, on_delta).await
        })
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

fn configured_max_tokens() -> Option<u16> {
    std::env::var("API_MAX_TOKENS")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<u16>().ok())
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
}
