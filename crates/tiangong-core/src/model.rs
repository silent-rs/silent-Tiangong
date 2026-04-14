use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;

use crate::mcp::build_mcp_tools_system_prompt;
use crate::session::{Message, MessageRole};
use anyhow::{Context, Result, anyhow};
use async_openai::Client as OpenAIClient;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tiangong_llm::message::{
    ChatMessage, MessageContent as LlmMessageContent, MessageRole as LlmMessageRole,
};
use tiangong_llm::provider::LlmProvider;
use tiangong_llm::providers::anthropic::{AnthropicConfig, AnthropicProvider};
use tiangong_llm::providers::openai::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use tiangong_llm::request::ProviderRequest;
use tiangong_llm::response::ProviderResponse;
use tiangong_llm::stream::{ProviderStream, ProviderStreamEvent};
use tiangong_llm::tool::{ToolCall as LlmToolCall, ToolChoice as LlmToolChoice, ToolSpec};
use tiangong_llm::usage::TokenUsageData;
use tokio::runtime::Builder as TokioRuntimeBuilder;

pub use tiangong_llm::ProviderProtocol;
pub use tiangong_types::TokenUsage;

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub session_title: String,
    pub user_input: String,
    pub context: Vec<Message>,
    /// 已由 PromptAssembler 装配的 system prompt。
    /// 设置此字段后 build_openai_messages 跳过自己的环境注入。
    pub assembled_system_prompt: Option<String>,
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
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub text: String,
    pub reasoning_content: String,
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

    fn block_on_llm<F, T>(&self, future: F) -> Result<T>
    where
        F: std::future::Future<Output = std::result::Result<T, tiangong_llm::error::LlmError>>,
    {
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
        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起 Anthropic 请求"));
        }

        let provider = self.build_anthropic_provider(timeout_ms)?;
        let request =
            build_anthropic_provider_request(req, model, anthropic_max_tokens(), functions);
        let response = self.block_on_llm(provider.complete(request))?;
        convert_provider_response_to_function_response(response)
    }

    fn complete_lite_anthropic(&self, prompt: &str) -> Result<String> {
        let timeout_ms = 30_000u64;
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
        let request = build_provider_request(req, model, anthropic_max_tokens(), &[]);
        let provider = match self.protocol() {
            ProviderProtocol::Anthropic => {
                ProviderDispatch::Anthropic(Box::new(self.build_anthropic_provider(timeout_ms)?))
            }
            ProviderProtocol::OpenAiCompatible => ProviderDispatch::OpenAi(Box::new(
                build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?,
            )),
        };
        consume_provider_stream(provider, request, &mut on_delta)
    }

    /// 流式函数调用：实时输出 thinking，同时累积 tool_calls
    pub fn complete_with_functions_stream_impl(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起流式工具模型请求"));
        }
        let request = build_provider_request(req, model, anthropic_max_tokens(), functions);
        let provider = match self.protocol() {
            ProviderProtocol::Anthropic => {
                ProviderDispatch::Anthropic(Box::new(self.build_anthropic_provider(timeout_ms)?))
            }
            ProviderProtocol::OpenAiCompatible => ProviderDispatch::OpenAi(Box::new(
                build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?,
            )),
        };
        convert_stream_to_function_response(provider, request, on_delta)
    }

    /// 使用轻量级模型完成简单任务（如会话名称生成）
    /// 如果未配置轻量级模型，则使用主模型
    /// 该方法使用更短的超时时间和较低温度以获得更确定的结果
    pub fn complete_lite(&self, prompt: &str) -> Result<String> {
        if self.protocol() == ProviderProtocol::Anthropic {
            return self.complete_lite_anthropic(prompt);
        }
        let timeout_ms = 30_000u64;
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
        };
        let response = self.block_on_llm(provider.complete(request))?;
        Ok(collect_provider_text(&response).trim().to_string())
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
        let request = build_provider_request(req, model, anthropic_max_tokens(), &[]);
        let response = self.block_on_llm(provider.complete(request))?;
        Ok(ModelResponse {
            text: collect_provider_text(&response).trim().to_string(),
            reasoning_content: response.reasoning_content.unwrap_or_default(),
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
        if self.protocol() == ProviderProtocol::Anthropic {
            return self.complete_with_functions_anthropic(req, functions);
        }

        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起工具模型请求"));
        }
        let provider =
            build_openai_provider_from_config(&self.cfg, timeout_ms, self.on_retry.clone())?;
        let request = build_provider_request(req, model, anthropic_max_tokens(), functions);
        let response = self.block_on_llm(provider.complete(request))?;
        convert_provider_response_to_function_response(response)
    }

    fn complete_with_functions_stream(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        if use_stream_mode() {
            match self.complete_with_functions_stream_impl(req, functions, on_delta) {
                Ok(resp) => Ok(resp),
                Err(_) => {
                    // 流式失败，回退到非流式，一次性推送 reasoning + content
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
        } else {
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
}

#[allow(dead_code)]
fn build_openai_messages(req: &ModelRequest) -> Result<Vec<ChatCompletionRequestMessage>> {
    let mut messages = Vec::new();

    // 如果已由 PromptAssembler 装配，使用装配好的 system prompt，不再自行注入环境信息
    let system_texts = if let Some(ref assembled) = req.assembled_system_prompt {
        let mut texts = vec![assembled.clone()];
        // context 中的 System 消息仍然追加（attachment 等）
        for msg in &req.context {
            if msg.role == MessageRole::System && !msg.content.trim().is_empty() {
                texts.push(msg.content.clone());
            }
        }
        // context 中的 User/Assistant 消息
        for msg in &req.context {
            match msg.role {
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
                _ => {}
            }
        }
        texts
    } else {
        // 旧路径：自行注入环境信息（兼容 TurnRunner / Worker）
        let mut texts = vec![
            format!("当前会话：{}", req.session_title),
            format!("当前工作目录：{}", current_working_directory_text()),
            format!("允许文件操作目录：{}", allowed_file_roots_text()),
        ];
        if let Some(mcp_tools_prompt) = build_mcp_tools_system_prompt(24) {
            texts.push(mcp_tools_prompt);
        }
        for msg in &req.context {
            match msg.role {
                MessageRole::System => {
                    if !msg.content.trim().is_empty() {
                        texts.push(msg.content.clone());
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
        texts
    };

    messages.insert(
        0,
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_texts.join("\n"))
            .build()
            .context("构建 system 消息失败")?
            .into(),
    );

    // 用户输入统一通过 context 传递（session history），不再单独追加。
    // 仅在旧路径（无 assembled_system_prompt）时追加 user_input 作为兼容。
    if req.assembled_system_prompt.is_none() && !req.user_input.is_empty() {
        messages.push(
            ChatCompletionRequestUserMessageArgs::default()
                .content(req.user_input.clone())
                .build()
                .context("构建当前 user 消息失败")?
                .into(),
        );
    }

    Ok(messages)
}

#[allow(dead_code)]
fn build_anthropic_request_body(
    req: &ModelRequest,
    model: &str,
    max_tokens: u32,
    functions: &[FunctionToolSpec],
    stream: bool,
) -> Result<Value> {
    let (system, messages) = build_anthropic_messages(req)?;
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
    });

    if !system.is_empty() {
        body["system"] = Value::String(system);
    }
    if stream {
        body["stream"] = Value::Bool(true);
    }
    if !functions.is_empty() {
        body["tools"] = Value::Array(
            functions
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect(),
        );
    }

    Ok(body)
}

#[allow(dead_code)]
fn build_anthropic_messages(req: &ModelRequest) -> Result<(String, Vec<Value>)> {
    let mut messages = Vec::new();

    let system_texts = if let Some(ref assembled) = req.assembled_system_prompt {
        let mut texts = vec![assembled.clone()];
        for msg in &req.context {
            if msg.role == MessageRole::System && !msg.content.trim().is_empty() {
                texts.push(msg.content.clone());
            }
        }
        for msg in &req.context {
            if let Some(payload) = anthropic_message_from_session(msg) {
                messages.push(payload);
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
            match msg.role {
                MessageRole::System => {
                    if !msg.content.trim().is_empty() {
                        texts.push(msg.content.clone());
                    }
                }
                _ => {
                    if let Some(payload) = anthropic_message_from_session(msg) {
                        messages.push(payload);
                    }
                }
            }
        }
        texts
    };

    if req.assembled_system_prompt.is_none() && !req.user_input.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": [{ "type": "text", "text": req.user_input }],
        }));
    }

    Ok((system_texts.join("\n"), messages))
}

#[allow(dead_code)]
fn anthropic_message_from_session(msg: &Message) -> Option<Value> {
    let role = match msg.role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => return None,
    };

    let mut parts = Vec::new();
    if !msg.content.trim().is_empty() {
        parts.push(msg.content.trim().to_string());
    }
    if !msg.reasoning_content.trim().is_empty() {
        parts.push(format!("[思考]\n{}", msg.reasoning_content.trim()));
    }
    if parts.is_empty() {
        return None;
    }

    Some(json!({
        "role": role,
        "content": [{ "type": "text", "text": parts.join("\n\n") }],
    }))
}

#[allow(dead_code)]
fn normalize_anthropic_base(api_base: &str) -> Result<String> {
    let trimmed = api_base.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(anyhow!("API_BASE_URL 不能为空"));
    }
    if trimmed.ends_with("/v1") {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("{trimmed}/v1"))
    }
}

#[allow(dead_code)]
fn anthropic_version_header() -> &'static str {
    "2023-06-01"
}

fn anthropic_max_tokens() -> u32 {
    configured_max_tokens().map(u32::from).unwrap_or(4096)
}

#[allow(dead_code)]
fn anthropic_post_json(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
    body: &Value,
    on_retry: &Option<OnRetryCallback>,
    label: &str,
) -> Result<Value> {
    let body_text = serde_json::to_string(body).context("序列化 Anthropic 请求失败")?;
    let response = with_retry_http(label, on_retry, || {
        let resp = client
            .post(url)
            .header("x-api-key", token)
            .header("anthropic-version", anthropic_version_header())
            .header("content-type", "application/json")
            .body(body_text.clone())
            .send()
            .map_err(map_reqwest_retry_error)?;
        anthropic_check_response(resp)
    })
    .map_err(|err| anyhow!(err.to_string()))?;

    let text = response.text().context("读取 Anthropic 响应失败")?;
    serde_json::from_str(&text).with_context(|| format!("解析 Anthropic 响应失败：{text}"))
}

#[allow(dead_code)]
fn anthropic_post_stream(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
    body: &Value,
    on_retry: &Option<OnRetryCallback>,
    label: &str,
) -> Result<reqwest::blocking::Response> {
    let body_text = serde_json::to_string(body).context("序列化 Anthropic 流式请求失败")?;
    with_retry_http(label, on_retry, || {
        let resp = client
            .post(url)
            .header("x-api-key", token)
            .header("anthropic-version", anthropic_version_header())
            .header("content-type", "application/json")
            .body(body_text.clone())
            .send()
            .map_err(map_reqwest_retry_error)?;
        anthropic_check_response(resp)
    })
    .map_err(|err| anyhow!(err.to_string()))
}

#[allow(dead_code)]
fn anthropic_check_response(
    resp: reqwest::blocking::Response,
) -> std::result::Result<reqwest::blocking::Response, HttpRetryError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }

    let retryable = status.as_u16() == 429 || status.is_server_error();
    let body = resp.text().unwrap_or_default();
    let message = extract_anthropic_error_message(&body)
        .unwrap_or_else(|| format!("Anthropic 请求失败：HTTP {status}，响应：{body}"));
    Err(HttpRetryError::new(message, retryable))
}

#[allow(dead_code)]
fn parse_anthropic_function_response(payload: &Value) -> Result<ModelFunctionResponse> {
    let mut text = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls = Vec::new();

    if let Some(content) = payload.get("content").and_then(Value::as_array) {
        for block in content {
            match block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "text" => {
                    if let Some(piece) = block.get("text").and_then(Value::as_str) {
                        text.push_str(piece);
                    }
                }
                "thinking" => {
                    if let Some(piece) = block.get("thinking").and_then(Value::as_str) {
                        reasoning_content.push_str(piece);
                    }
                }
                "tool_use" => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    if !name.is_empty() {
                        tool_calls.push(ModelFunctionCall {
                            id,
                            name,
                            arguments,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    let input_tokens = payload
        .get("usage")
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let output_tokens = payload
        .get("usage")
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    if text.trim().is_empty() && reasoning_content.trim().is_empty() && tool_calls.is_empty() {
        return Err(anyhow!("Anthropic 响应缺少文本和工具调用"));
    }

    Ok(ModelFunctionResponse {
        text: text.trim().to_string(),
        reasoning_content: reasoning_content.trim().to_string(),
        usage: TokenUsage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
            total_tokens: input_tokens + output_tokens,
        },
        tool_calls,
    })
}

#[allow(dead_code)]
fn parse_anthropic_stream_response(
    resp: reqwest::blocking::Response,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ModelFunctionResponse> {
    let mut reader = BufReader::new(resp);
    let mut current_event = String::new();
    let mut data_lines: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut reasoning_content = String::new();
    let mut input_tokens = 0usize;
    let mut output_tokens = 0usize;
    let mut tool_calls_map: std::collections::BTreeMap<u64, (String, String, String)> =
        std::collections::BTreeMap::new();

    let mut process_payload = |event_name: &str, raw_data: &str| -> Result<()> {
        if raw_data.trim().is_empty() || raw_data.trim() == "[DONE]" || event_name == "ping" {
            return Ok(());
        }

        let payload: Value = serde_json::from_str(raw_data)
            .with_context(|| format!("解析 Anthropic SSE 事件失败：{raw_data}"))?;
        let payload_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(event_name);

        if let Some(usage) = payload.get("usage") {
            if let Some(v) = usage.get("input_tokens").and_then(Value::as_u64) {
                input_tokens = v as usize;
            }
            if let Some(v) = usage.get("output_tokens").and_then(Value::as_u64) {
                output_tokens = v as usize;
            }
        }
        if let Some(message_usage) = payload
            .get("message")
            .and_then(|message| message.get("usage"))
        {
            if let Some(v) = message_usage.get("input_tokens").and_then(Value::as_u64) {
                input_tokens = v as usize;
            }
            if let Some(v) = message_usage.get("output_tokens").and_then(Value::as_u64) {
                output_tokens = v as usize;
            }
        }

        match payload_type {
            "content_block_start" => {
                let index = payload.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(block) = payload.get("content_block") {
                    match block
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let input = block
                                .get("input")
                                .and_then(|input| {
                                    if input.is_object() {
                                        Some(input.to_string())
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or_default();
                            tool_calls_map.insert(index, (id, name, input));
                        }
                        "thinking" => {
                            if let Some(piece) = block.get("thinking").and_then(Value::as_str) {
                                reasoning_content.push_str(piece);
                                on_delta(&ModelStreamChunk {
                                    content: String::new(),
                                    reasoning_content: piece.to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                let index = payload.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(delta) = payload.get("delta") {
                    match delta
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "text_delta" => {
                            if let Some(piece) = delta.get("text").and_then(Value::as_str) {
                                text.push_str(piece);
                                on_delta(&ModelStreamChunk {
                                    content: piece.to_string(),
                                    reasoning_content: String::new(),
                                });
                            }
                        }
                        "thinking_delta" => {
                            if let Some(piece) = delta.get("thinking").and_then(Value::as_str) {
                                reasoning_content.push_str(piece);
                                on_delta(&ModelStreamChunk {
                                    content: String::new(),
                                    reasoning_content: piece.to_string(),
                                });
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) = delta.get("partial_json").and_then(Value::as_str)
                            {
                                let entry = tool_calls_map.entry(index).or_insert_with(|| {
                                    (String::new(), String::new(), String::new())
                                });
                                entry.2.push_str(partial);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        Ok(())
    };

    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("读取 Anthropic SSE 流失败")?;
        if read == 0 {
            break;
        }

        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !data_lines.is_empty() {
                process_payload(&current_event, &data_lines.join("\n"))?;
                current_event.clear();
                data_lines.clear();
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("event:") {
            current_event = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start().to_string());
        }
    }

    if !data_lines.is_empty() {
        process_payload(&current_event, &data_lines.join("\n"))?;
    }

    let tool_calls = tool_calls_map
        .into_values()
        .filter(|(_, name, _)| !name.is_empty())
        .map(|(id, name, args)| {
            let arguments = serde_json::from_str::<Value>(&args)
                .ok()
                .filter(|value| value.is_object())
                .unwrap_or_else(|| json!({}));
            ModelFunctionCall {
                id,
                name,
                arguments,
            }
        })
        .collect::<Vec<_>>();

    if text.trim().is_empty() && reasoning_content.trim().is_empty() && tool_calls.is_empty() {
        return Err(anyhow!("Anthropic 流式响应缺少文本和工具调用"));
    }

    Ok(ModelFunctionResponse {
        text: text.trim().to_string(),
        reasoning_content: reasoning_content.trim().to_string(),
        usage: TokenUsage {
            prompt_tokens: input_tokens,
            completion_tokens: output_tokens,
            total_tokens: input_tokens + output_tokens,
        },
        tool_calls,
    })
}

#[allow(dead_code)]
fn extract_anthropic_error_message(body: &str) -> Option<String> {
    let payload: Value = serde_json::from_str(body).ok()?;
    let message = payload
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)?;
    Some(format!("Anthropic 请求失败：{message}"))
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

fn build_anthropic_provider_request(
    req: &ModelRequest,
    model: &str,
    max_tokens: u32,
    functions: &[FunctionToolSpec],
) -> ProviderRequest {
    build_provider_request(req, model, max_tokens, functions)
}

fn build_provider_request(
    req: &ModelRequest,
    model: &str,
    max_tokens: u32,
    functions: &[FunctionToolSpec],
) -> ProviderRequest {
    let (system, messages) = build_provider_messages(req);
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
        tool_choice: (!functions.is_empty()).then_some(LlmToolChoice::Auto),
        max_tokens: Some(max_tokens),
        temperature: configured_temperature_f32(),
        top_p: None,
        stop_sequences: Vec::new(),
        metadata: None,
    }
}

fn build_provider_messages(req: &ModelRequest) -> (String, Vec<ChatMessage>) {
    let mut messages = Vec::new();

    let system_texts = if let Some(ref assembled) = req.assembled_system_prompt {
        let mut texts = vec![assembled.clone()];
        for msg in &req.context {
            if msg.role == MessageRole::System && !msg.content.trim().is_empty() {
                texts.push(msg.content.clone());
            }
        }
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
            match msg.role {
                MessageRole::System => {
                    if !msg.content.trim().is_empty() {
                        texts.push(msg.content.clone());
                    }
                }
                _ => {
                    if let Some(message) = provider_message_from_session(msg) {
                        messages.push(message);
                    }
                }
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

    (system_texts.join("\n"), messages)
}

fn provider_message_from_session(msg: &Message) -> Option<ChatMessage> {
    let role = match msg.role {
        MessageRole::User => LlmMessageRole::User,
        MessageRole::Assistant => LlmMessageRole::Assistant,
        MessageRole::System => return None,
    };

    let mut parts = Vec::new();
    if !msg.content.trim().is_empty() {
        parts.push(msg.content.trim().to_string());
    }
    if !msg.reasoning_content.trim().is_empty() {
        parts.push(format!("[思考]\n{}", msg.reasoning_content.trim()));
    }
    if parts.is_empty() {
        return None;
    }

    Some(ChatMessage::new(
        role,
        vec![LlmMessageContent::Text(parts.join("\n\n"))],
    ))
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
        usage: response.usage.unwrap_or_default().into(),
        tool_calls,
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

fn consume_anthropic_stream(
    stream: ProviderStream,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ModelFunctionResponse> {
    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化异步运行时失败")?;

    runtime.block_on(async {
        let mut stream = stream;
        let mut text = String::new();
        let mut usage = TokenUsageData::default();
        let mut tool_calls: std::collections::BTreeMap<String, (String, String)> =
            std::collections::BTreeMap::new();

        while let Some(event) = stream.next().await {
            match event.map_err(map_llm_error)? {
                ProviderStreamEvent::ReasoningDelta(delta) => {
                    if !delta.is_empty() {
                        on_delta(&ModelStreamChunk {
                            content: String::new(),
                            reasoning_content: delta.clone(),
                        });
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
                id,
                name,
                arguments: if raw_args.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&raw_args).unwrap_or_else(|_| json!({}))
                },
            })
            .collect::<Vec<_>>();

        if text.trim().is_empty() && tool_calls.is_empty() {
            return Err(anyhow!("Anthropic 流式响应缺少文本和工具调用"));
        }

        Ok(ModelFunctionResponse {
            text: text.trim().to_string(),
            reasoning_content: String::new(),
            usage: usage.into(),
            tool_calls,
        })
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
    let stream = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化异步运行时失败")?
        .block_on(provider.stream(request))
        .map_err(map_llm_error)?;

    let response = consume_anthropic_stream(stream, on_delta)?;
    Ok(ModelResponse {
        text: response.text,
        reasoning_content: response.reasoning_content,
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
    let stream = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化异步运行时失败")?
        .block_on(provider.stream(request))
        .map_err(map_llm_error)?;
    consume_anthropic_stream(stream, on_delta)
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

/// 规范化 API 基础地址
///
/// 仅做基本清理（去空格、去尾部斜杠、去意外拼接的 /chat/completions），
/// 不自动补充版本路径——版本由用户在 provider base_url 中指定。
#[allow(dead_code)]
fn normalize_api_base(base_url: &str) -> Result<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("API_BASE_URL 不能为空"));
    }

    let cleaned = trimmed.trim_end_matches('/');
    let cleaned = cleaned.strip_suffix("/chat/completions").unwrap_or(cleaned);
    Ok(cleaned.to_string())
}

/// 创建禁用内部 backoff 的 OpenAI 客户端
///
/// async-openai 默认有 ExponentialBackoff（最长 15 分钟）。
/// 禁用后由 `with_retry` 统一管理重试，确保 StreamEvent::Retry 能正确触发。
#[allow(dead_code)]
fn build_no_retry_client(config: OpenAIConfig) -> OpenAIClient<OpenAIConfig> {
    // 设置极小的 max_elapsed_time 确保 async-openai 不做任何内部重试。
    // Duration::ZERO 在 backoff 首次调用时 elapsed ≈ 0 可能仍允许 1 次重试，
    // 使用 1ns 确保 elapsed > max_elapsed_time 立即停止。
    let no_retry = backoff::ExponentialBackoff {
        max_elapsed_time: Some(Duration::from_nanos(1)),
        ..Default::default()
    };
    OpenAIClient::build(reqwest::Client::new(), config, no_retry)
}

#[allow(dead_code)]
fn build_sdk_error_hint(error_text: &str) -> String {
    if error_text.contains("/chat/completions") && error_text.contains("404") {
        return "；请检查 API_BASE_URL 是否包含正确的版本路径（如 .../v1）".to_string();
    }
    if error_text.contains("expected struct ApiError") {
        return "；当前网关返回的错误结构非 OpenAI 标准格式，请确认 API_BASE_URL 是否为 OpenAI 兼容接口".to_string();
    }
    String::new()
}

// ── 重试相关 ──────────────────────────────────────────────

/// 最大重试次数
const MAX_RETRIES: u32 = 3;
/// 初始重试延迟（毫秒）
const INITIAL_RETRY_DELAY_MS: u64 = 1000;
/// 退避倍率
const RETRY_BACKOFF_MULTIPLIER: u64 = 2;

#[derive(Debug)]
#[allow(dead_code)]
struct HttpRetryError {
    message: String,
    retryable: bool,
}

impl HttpRetryError {
    fn new(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            retryable,
        }
    }
}

impl std::fmt::Display for HttpRetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for HttpRetryError {}

/// 判断 OpenAI SDK 错误是否可重试
#[allow(dead_code)]
fn is_retryable_openai_error(err: &async_openai::error::OpenAIError) -> bool {
    is_retryable_error_text(&err.to_string())
}

#[allow(dead_code)]
fn map_reqwest_retry_error(err: reqwest::Error) -> HttpRetryError {
    let retryable = err.is_timeout() || err.is_connect() || err.is_request();
    HttpRetryError::new(format!("HTTP 请求失败：{err}"), retryable)
}

/// 判断错误文本是否表示可重试的错误
#[allow(dead_code)]
fn is_retryable_error_text(text: &str) -> bool {
    // 速率限制 (HTTP 429)
    if text.contains("429")
        || text.contains("Rate limit")
        || text.contains("rate limit")
        || text.contains("Rate limited")
        || text.contains("访问量过大")
        || text.contains("稍后再试")
        || text.contains("too many requests")
        || text.contains("Too Many Requests")
    {
        return true;
    }
    // 服务端错误 (5xx)
    if text.contains("500 Internal Server Error")
        || text.contains("502 Bad Gateway")
        || text.contains("503 Service Unavailable")
        || text.contains("504 Gateway Timeout")
    {
        return true;
    }
    // 连接错误
    if text.contains("connection reset")
        || text.contains("connection refused")
        || text.contains("Connection reset")
        || text.contains("Connection refused")
    {
        return true;
    }
    false
}

/// 带重试的异步调用（用于 OpenAI SDK 请求）
///
/// 遇到速率限制、5xx、连接错误时自动重试，采用指数退避策略。
/// 可选的 `on_retry` 回调在每次重试前触发，用于通知上层。
#[allow(dead_code)]
async fn with_retry<F, Fut, T>(
    label: &str,
    on_retry: &Option<OnRetryCallback>,
    mut f: F,
) -> Result<T, async_openai::error::OpenAIError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, async_openai::error::OpenAIError>>,
{
    let mut attempt = 0u32;
    let mut delay_ms = INITIAL_RETRY_DELAY_MS;
    loop {
        match f().await {
            Ok(result) => return Ok(result),
            Err(err) => {
                if attempt < MAX_RETRIES && is_retryable_openai_error(&err) {
                    attempt += 1;
                    let err_text = err.to_string();
                    tracing::warn!(
                        attempt = attempt,
                        max_retries = MAX_RETRIES,
                        delay_ms = delay_ms,
                        error = %err_text,
                        label = label,
                        "LLM 请求失败，准备重试",
                    );
                    if let Some(cb) = on_retry {
                        cb(attempt, MAX_RETRIES, delay_ms, &err_text);
                    }
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms *= RETRY_BACKOFF_MULTIPLIER;
                } else {
                    return Err(err);
                }
            }
        }
    }
}

#[allow(dead_code)]
fn with_retry_http<F, T>(
    label: &str,
    on_retry: &Option<OnRetryCallback>,
    mut f: F,
) -> std::result::Result<T, HttpRetryError>
where
    F: FnMut() -> std::result::Result<T, HttpRetryError>,
{
    let mut attempt = 0u32;
    let mut delay_ms = INITIAL_RETRY_DELAY_MS;
    loop {
        match f() {
            Ok(result) => return Ok(result),
            Err(err) => {
                if attempt < MAX_RETRIES && err.retryable {
                    attempt += 1;
                    tracing::warn!(
                        attempt = attempt,
                        max_retries = MAX_RETRIES,
                        delay_ms = delay_ms,
                        error = %err,
                        label = label,
                        "LLM 请求失败，准备重试",
                    );
                    if let Some(cb) = on_retry {
                        cb(attempt, MAX_RETRIES, delay_ms, &err.to_string());
                    }
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    delay_ms *= RETRY_BACKOFF_MULTIPLIER;
                } else {
                    return Err(err);
                }
            }
        }
    }
}

#[allow(dead_code)]
fn extract_delta_text(delta: &Value, field: &str) -> String {
    let Some(raw) = delta.get(field) else {
        return String::new();
    };

    match raw {
        Value::String(value) => value.clone(),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                if let Some(value) = item.as_str() {
                    out.push_str(value);
                    continue;
                }
                if let Some(value) = item.get("text").and_then(Value::as_str) {
                    out.push_str(value);
                    continue;
                }
                if let Some(value) = item.get("content").and_then(Value::as_str) {
                    out.push_str(value);
                }
            }
            out
        }
        _ => String::new(),
    }
}

#[allow(dead_code)]
fn push_stream_piece(
    piece: ModelStreamChunk,
    on_delta: &mut impl FnMut(&ModelStreamChunk),
    text: &mut String,
    reasoning_content: &mut String,
    chunks: &mut usize,
) {
    if piece.content.is_empty() && piece.reasoning_content.is_empty() {
        return;
    }
    on_delta(&piece);
    text.push_str(&piece.content);
    reasoning_content.push_str(&piece.reasoning_content);
    *chunks += 1;
}

/// 流式 `<think>` 标签过滤器
///
/// 某些模型不使用 API 级别的 `reasoning_content` 字段，
/// 而是在 `content` 中输出 `<think>...</think>` 标签。
/// 此过滤器跨 chunk 追踪状态，将标签内的内容转移到 `reasoning_content`。
#[allow(dead_code)]
struct ThinkTagFilter {
    /// 是否处于 `<think>` 块内
    inside_think: bool,
    /// 部分匹配缓冲（可能跨 chunk 的标签片段）
    buf: String,
}

#[allow(dead_code)]
impl ThinkTagFilter {
    fn new() -> Self {
        Self {
            inside_think: false,
            buf: String::new(),
        }
    }

    /// 过滤输入的 content，返回 (正文内容, 思考内容)
    fn filter(&mut self, input: &str) -> (String, String) {
        let mut content = String::new();
        let mut reasoning = String::new();

        self.buf.push_str(input);

        while !self.buf.is_empty() {
            if self.inside_think {
                // 在 <think> 块内，寻找 </think>
                if let Some(pos) = self.buf.find("</think>") {
                    reasoning.push_str(&self.buf[..pos]);
                    self.buf = self.buf[pos + 8..].to_string();
                    self.inside_think = false;
                } else if self.buf.len() >= 8
                    && !self.buf.ends_with('<')
                    && !self.buf.ends_with("</")
                    && !self.buf.ends_with("</t")
                    && !self.buf.ends_with("</th")
                    && !self.buf.ends_with("</thi")
                    && !self.buf.ends_with("</thin")
                    && !self.buf.ends_with("</think")
                {
                    // 没有部分匹配前缀，全部输出为 reasoning
                    reasoning.push_str(&self.buf);
                    self.buf.clear();
                } else {
                    // 可能有部分 </think> 标签，保留缓冲等下一个 chunk
                    break;
                }
            } else {
                // 在正常内容中，寻找 <think>
                if let Some(pos) = self.buf.find("<think>") {
                    content.push_str(&self.buf[..pos]);
                    self.buf = self.buf[pos + 7..].to_string();
                    self.inside_think = true;
                } else if self.buf.len() >= 7
                    && !self.buf.ends_with('<')
                    && !self.buf.ends_with("<t")
                    && !self.buf.ends_with("<th")
                    && !self.buf.ends_with("<thi")
                    && !self.buf.ends_with("<thin")
                    && !self.buf.ends_with("<think")
                {
                    // 没有部分匹配前缀，全部输出为 content
                    content.push_str(&self.buf);
                    self.buf.clear();
                } else {
                    // 可能有部分 <think> 标签，保留缓冲
                    break;
                }
            }
        }

        (content, reasoning)
    }

    /// 流结束时刷出缓冲区残留
    fn flush(&mut self) -> (String, String) {
        let remaining = std::mem::take(&mut self.buf);
        if self.inside_think {
            (String::new(), remaining)
        } else {
            (remaining, String::new())
        }
    }
}

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

#[allow(dead_code)]
fn should_skip_stream_payload(raw: &str) -> bool {
    let normalized = raw.trim().to_ascii_lowercase();
    normalized.is_empty()
        || normalized == "[done]"
        || normalized == "ping"
        || normalized == "pong"
        || normalized.contains("\"event\":\"ping\"")
}

#[allow(dead_code)]
fn inject_thinking_config(payload: &mut Value) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };

    let thinking = serde_json::json!({
        "type": "enabled",
        "clear_thinking": clear_thinking(),
    });
    obj.insert("thinking".to_string(), thinking);
}

#[allow(dead_code)]
fn clear_thinking() -> bool {
    match std::env::var("API_CLEAR_THINKING") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

/// 注入 stream_options.include_usage = true，使流式响应最后一个 chunk 返回 usage 数据
#[allow(dead_code)]
fn inject_stream_usage_option(payload: &mut Value) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("stream_options".to_string(), json!({"include_usage": true}));
}

#[allow(dead_code)]
fn inject_temperature_config(payload: &mut Value) {
    let Some(temp) = configured_temperature_number() else {
        return;
    };
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("temperature".to_string(), Value::Number(temp));
}

#[allow(dead_code)]
fn inject_function_tools(payload: &mut Value, functions: &[FunctionToolSpec]) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    let tools = functions
        .iter()
        .map(|spec| {
            json!({
                "type": "function",
                "function": {
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": spec.parameters,
                }
            })
        })
        .collect::<Vec<_>>();
    obj.insert("tools".to_string(), Value::Array(tools));
    obj.insert("tool_choice".to_string(), Value::String("auto".to_string()));
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

#[allow(dead_code)]
fn parse_non_stream_byot_response(payload: &Value) -> Option<ModelResponse> {
    let choice = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())?;
    let message = choice.get("message")?;

    let reasoning = extract_delta_text(message, "reasoning_content");
    let content = extract_delta_text(message, "content");
    let text = content.trim().to_string();
    let reasoning_content = reasoning.trim().to_string();
    if text.is_empty() && reasoning_content.is_empty() {
        return None;
    }

    let usage = payload.get("usage");
    let prompt_tokens = usage
        .and_then(|v| v.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|v| v.get("completion_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);
    let total_tokens = usage
        .and_then(|v| v.get("total_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);

    Some(ModelResponse {
        text,
        reasoning_content,
        usage: TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        },
        output_mode: if reasoning.trim().is_empty() {
            "non-stream".to_string()
        } else {
            "non-stream-think".to_string()
        },
        output_chunk_count: 1,
    })
}

#[allow(dead_code)]
fn parse_function_byot_response(payload: &Value) -> Result<ModelFunctionResponse> {
    let choice = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| anyhow!("工具调用响应缺少 choices"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| anyhow!("工具调用响应缺少 message"))?;

    let text = extract_delta_text(message, "content").trim().to_string();
    let reasoning_content = extract_delta_text(message, "reasoning_content")
        .trim()
        .to_string();
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(parse_function_call_item)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if text.is_empty() && reasoning_content.is_empty() && tool_calls.is_empty() {
        return Err(anyhow!("工具调用响应既没有文本也没有函数调用"));
    }

    let usage = payload.get("usage");
    let prompt_tokens = usage
        .and_then(|v| v.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|v| v.get("completion_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);
    let total_tokens = usage
        .and_then(|v| v.get("total_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(0);

    Ok(ModelFunctionResponse {
        text,
        reasoning_content,
        usage: TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        },
        tool_calls,
    })
}

#[allow(dead_code)]
fn parse_function_call_item(item: &Value) -> Option<ModelFunctionCall> {
    let name = item
        .get("function")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())?
        .to_string();
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let raw_args = item
        .get("function")
        .and_then(|v| v.get("arguments"))
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let arguments = serde_json::from_str::<Value>(raw_args)
        .ok()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| Value::Object(Map::new()));

    Some(ModelFunctionCall {
        id,
        name,
        arguments,
    })
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
