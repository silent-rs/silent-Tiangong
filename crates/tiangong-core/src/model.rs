use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_openai::Client as OpenAIClient;
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs, ChatThinking,
    ChatThinkingType, CreateChatCompletionRequestArgs, CreateChatCompletionResponse,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use crate::mcp::build_mcp_tools_system_prompt;
use crate::session::{Message, MessageRole};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

impl TokenUsage {
    /// 累加另一个 TokenUsage 到自身
    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
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
        let api_model = std::env::var("API_MODEL").unwrap_or_else(|_| default_api_model());
        let api_lite_model = std::env::var("API_LITE_MODEL").unwrap_or_default();
        Self {
            api_auth_token,
            api_base_url,
            api_timeout_ms,
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

        // 直接发 HTTP 请求而非使用 SDK 的 models().list()，
        // 因为部分 API 供应商（如 DeepSeek）返回的模型对象缺少 `created` 等字段，
        // SDK 的严格反序列化会失败。
        let url = format!("{api_base}/models");
        let http_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .context("创建 HTTP 客户端失败")?;

        let resp = http_client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .with_context(|| format!("请求模型列表失败：{url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("获取模型列表失败：HTTP {status}，响应：{body}"));
        }

        let body = resp.text().context("读取模型列表响应失败")?;

        // 宽松反序列化：只需要 id 字段
        #[derive(serde::Deserialize)]
        struct ModelEntry {
            id: String,
        }
        #[derive(serde::Deserialize)]
        struct ModelsResponse {
            data: Vec<ModelEntry>,
        }

        let parsed: ModelsResponse = serde_json::from_str(&body)
            .with_context(|| format!("failed to deserialize api response: {body}"))?;

        let mut models = parsed.data.into_iter().map(|m| m.id).collect::<Vec<_>>();
        models.sort();
        models.dedup();
        Ok(models)
    }

    pub fn complete_stream_with_callback<F>(
        &self,
        req: &ModelRequest,
        mut on_delta: F,
    ) -> Result<ModelResponse>
    where
        F: FnMut(&ModelStreamChunk),
    {
        let token = self.cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起流式模型请求"));
        }

        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起流式模型请求"));
        }

        let api_base = normalize_api_base(&self.cfg.api_base_url)?;
        let messages = build_openai_messages(req)?;
        let mut request_args_binding = CreateChatCompletionRequestArgs::default();
        let mut request_args = request_args_binding
            .model(model.to_string())
            .messages(messages);
        if let Some(max_tokens) = configured_max_tokens() {
            request_args = request_args.max_tokens(max_tokens);
        }
        let mut request = request_args.build().context("构建 OpenAI 流式请求失败")?;
        request.stream = Some(true);

        let config = OpenAIConfig::new()
            .with_api_key(token.to_string())
            .with_api_base(api_base);
        let client = OpenAIClient::with_config(config);
        let mut request_json = serde_json::to_value(&request).context("序列化流式请求失败")?;
        inject_temperature_config(&mut request_json);
        inject_thinking_config(&mut request_json);
        inject_stream_usage_option(&mut request_json);

        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;

        let response = runtime.block_on(async {
            timeout(Duration::from_millis(timeout_ms), async {
                let mut stream = client
                    .chat()
                    .create_stream_byot::<_, Value>(request_json)
                    .await?;
                let mut content = String::new();
                let mut reasoning_content = String::new();
                let mut chunks = 0usize;
                let mut has_think_output = false;
                let mut stream_usage = (0usize, 0usize, 0usize); // (prompt, completion, total)

                while let Some(item) = stream.next().await {
                    let payload = match item {
                        Ok(payload) => payload,
                        Err(err) => {
                            if let async_openai::error::OpenAIError::JSONDeserialize(_, raw) = &err
                                && should_skip_stream_payload(raw)
                            {
                                continue;
                            }
                            return Err(err);
                        }
                    };

                    // 提取流式最后 chunk 中的 usage 数据
                    if let Some(usage) = payload.get("usage") {
                        if let Some(v) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                            stream_usage.0 = v as usize;
                        }
                        if let Some(v) = usage.get("completion_tokens").and_then(Value::as_u64) {
                            stream_usage.1 = v as usize;
                        }
                        if let Some(v) = usage.get("total_tokens").and_then(Value::as_u64) {
                            stream_usage.2 = v as usize;
                        }
                    }

                    if let Some(choices) = payload.get("choices").and_then(Value::as_array) {
                        for choice in choices {
                            let delta = choice.get("delta").unwrap_or(&Value::Null);

                            let think_delta = extract_delta_text(delta, "reasoning_content");
                            if !think_delta.is_empty() {
                                has_think_output = true;
                                push_stream_piece(
                                    ModelStreamChunk {
                                        content: String::new(),
                                        reasoning_content: think_delta.clone(),
                                    },
                                    &mut on_delta,
                                    &mut content,
                                    &mut reasoning_content,
                                    &mut chunks,
                                );
                            }

                            let content_delta = extract_delta_text(delta, "content");
                            if !content_delta.is_empty() {
                                push_stream_piece(
                                    ModelStreamChunk {
                                        content: content_delta.clone(),
                                        reasoning_content: String::new(),
                                    },
                                    &mut on_delta,
                                    &mut content,
                                    &mut reasoning_content,
                                    &mut chunks,
                                );
                            }
                        }
                    }
                }

                Ok::<
                    (String, String, usize, bool, (usize, usize, usize)),
                    async_openai::error::OpenAIError,
                >((
                    content,
                    reasoning_content,
                    chunks,
                    has_think_output,
                    stream_usage,
                ))
            })
            .await
        });

        let byot_outcome = match response {
            Ok(Ok(payload)) => Some(payload),
            Ok(Err(_)) => None,
            Err(_) => None,
        };

        if let Some((text, reasoning_content, chunks, has_think_output, stream_usage)) =
            byot_outcome
        {
            let text = text.trim().to_string();
            let reasoning_content = reasoning_content.trim().to_string();
            if (!text.is_empty() || !reasoning_content.is_empty()) && chunks > 0 {
                return Ok(ModelResponse {
                    text,
                    reasoning_content,
                    usage: TokenUsage {
                        prompt_tokens: stream_usage.0,
                        completion_tokens: stream_usage.1,
                        total_tokens: stream_usage.2,
                    },
                    output_mode: if has_think_output {
                        "stream-think".to_string()
                    } else {
                        "stream".to_string()
                    },
                    output_chunk_count: chunks.max(1),
                });
            }
        }

        // 当流式 BYOT 路径失败时，转为非流式补偿请求，避免吞掉 reasoning_content。
        let fallback_resp = self.complete(req)?;
        if !fallback_resp.reasoning_content.is_empty() {
            on_delta(&ModelStreamChunk {
                content: String::new(),
                reasoning_content: fallback_resp.reasoning_content.clone(),
            });
        }
        if !fallback_resp.text.is_empty() {
            on_delta(&ModelStreamChunk {
                content: fallback_resp.text.clone(),
                reasoning_content: String::new(),
            });
        }
        Ok(fallback_resp)
    }

    /// 流式函数调用：实时输出 thinking，同时累积 tool_calls
    pub fn complete_with_functions_stream_impl(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
        on_delta: &mut dyn FnMut(&ModelStreamChunk),
    ) -> Result<ModelFunctionResponse> {
        let token = self.cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起流式工具模型请求"));
        }

        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起流式工具模型请求"));
        }

        let api_base = normalize_api_base(&self.cfg.api_base_url)?;
        let messages = build_openai_messages(req)?;
        let mut request_args_binding = CreateChatCompletionRequestArgs::default();
        let request_args = request_args_binding
            .model(model.to_string())
            .messages(messages);
        let mut request = request_args
            .build()
            .context("构建流式 function call 请求失败")?;
        request.stream = Some(true);

        let config = OpenAIConfig::new()
            .with_api_key(token.to_string())
            .with_api_base(api_base);
        let client = OpenAIClient::with_config(config);
        let mut request_json = serde_json::to_value(&request).context("序列化请求失败")?;
        inject_temperature_config(&mut request_json);
        inject_thinking_config(&mut request_json);
        inject_stream_usage_option(&mut request_json);
        inject_function_tools(&mut request_json, functions);

        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;

        let response = runtime.block_on(async {
            timeout(Duration::from_millis(timeout_ms), async {
                let mut stream = client
                    .chat()
                    .create_stream_byot::<_, Value>(request_json)
                    .await?;

                let mut content = String::new();
                let mut reasoning_content = String::new();
                // tool_calls 按 index 累积：index -> (id, name, arguments)
                let mut tool_calls_map: std::collections::BTreeMap<u64, (String, String, String)> =
                    std::collections::BTreeMap::new();
                let mut stream_usage = (0usize, 0usize, 0usize);

                while let Some(item) = stream.next().await {
                    let payload = match item {
                        Ok(payload) => payload,
                        Err(err) => {
                            if let async_openai::error::OpenAIError::JSONDeserialize(_, raw) = &err
                                && should_skip_stream_payload(raw)
                            {
                                continue;
                            }
                            return Err(err);
                        }
                    };

                    // 提取流式最后 chunk 中的 usage 数据
                    if let Some(usage) = payload.get("usage") {
                        if let Some(v) = usage.get("prompt_tokens").and_then(Value::as_u64) {
                            stream_usage.0 = v as usize;
                        }
                        if let Some(v) = usage.get("completion_tokens").and_then(Value::as_u64) {
                            stream_usage.1 = v as usize;
                        }
                        if let Some(v) = usage.get("total_tokens").and_then(Value::as_u64) {
                            stream_usage.2 = v as usize;
                        }
                    }

                    if let Some(choices) = payload.get("choices").and_then(Value::as_array) {
                        for choice in choices {
                            let delta = choice.get("delta").unwrap_or(&Value::Null);

                            // 流式输出 reasoning_content
                            let think_delta = extract_delta_text(delta, "reasoning_content");
                            if !think_delta.is_empty() {
                                reasoning_content.push_str(&think_delta);
                                on_delta(&ModelStreamChunk {
                                    content: String::new(),
                                    reasoning_content: think_delta,
                                });
                            }

                            // 流式输出 content
                            let content_delta = extract_delta_text(delta, "content");
                            if !content_delta.is_empty() {
                                content.push_str(&content_delta);
                                on_delta(&ModelStreamChunk {
                                    content: content_delta,
                                    reasoning_content: String::new(),
                                });
                            }

                            // 累积 tool_calls
                            if let Some(tool_calls) =
                                delta.get("tool_calls").and_then(Value::as_array)
                            {
                                for tc in tool_calls {
                                    let index =
                                        tc.get("index").and_then(Value::as_u64).unwrap_or(0);
                                    let entry = tool_calls_map.entry(index).or_insert_with(|| {
                                        (String::new(), String::new(), String::new())
                                    });
                                    if let Some(id) = tc.get("id").and_then(Value::as_str) {
                                        entry.0 = id.to_string();
                                    }
                                    if let Some(func) = tc.get("function") {
                                        if let Some(name) = func.get("name").and_then(Value::as_str)
                                        {
                                            entry.1 = name.to_string();
                                        }
                                        if let Some(args) =
                                            func.get("arguments").and_then(Value::as_str)
                                        {
                                            entry.2.push_str(args);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                Ok::<_, async_openai::error::OpenAIError>((
                    content,
                    reasoning_content,
                    tool_calls_map,
                    stream_usage,
                ))
            })
            .await
        });

        match response {
            Ok(Ok((text, reasoning, tool_calls_map, stream_usage))) => {
                let tool_calls = tool_calls_map
                    .into_values()
                    .filter(|(_, name, _)| !name.is_empty())
                    .map(|(id, name, args)| {
                        let arguments = serde_json::from_str::<Value>(&args)
                            .ok()
                            .filter(|v| v.is_object())
                            .unwrap_or_else(|| Value::Object(Map::new()));
                        ModelFunctionCall {
                            id,
                            name,
                            arguments,
                        }
                    })
                    .collect::<Vec<_>>();

                let text = text.trim().to_string();
                let reasoning_content = reasoning.trim().to_string();

                if text.is_empty() && reasoning_content.is_empty() && tool_calls.is_empty() {
                    // 流式路径未获取到有效内容，回退到非流式
                    let fallback = self.complete_with_functions(req, functions)?;
                    if !fallback.reasoning_content.is_empty() {
                        on_delta(&ModelStreamChunk {
                            content: String::new(),
                            reasoning_content: fallback.reasoning_content.clone(),
                        });
                    }
                    return Ok(fallback);
                }

                Ok(ModelFunctionResponse {
                    text,
                    reasoning_content,
                    usage: TokenUsage {
                        prompt_tokens: stream_usage.0,
                        completion_tokens: stream_usage.1,
                        total_tokens: stream_usage.2,
                    },
                    tool_calls,
                })
            }
            _ => {
                // 流式失败，回退到非流式
                let fallback = self.complete_with_functions(req, functions)?;
                if !fallback.reasoning_content.is_empty() {
                    on_delta(&ModelStreamChunk {
                        content: String::new(),
                        reasoning_content: fallback.reasoning_content.clone(),
                    });
                }
                Ok(fallback)
            }
        }
    }

    /// 使用轻量级模型完成简单任务（如会话名称生成）
    /// 如果未配置轻量级模型，则使用主模型
    /// 该方法使用更短的超时时间和较低温度以获得更确定的结果
    pub fn complete_lite(&self, prompt: &str) -> Result<String> {
        use tracing::{info, warn};

        let token = self.cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起轻量级模型请求"));
        }

        // 轻量级任务使用更短的超时（30 秒）
        let timeout_ms = 30_000u64;
        // lite_model() 会自动回退到主模型
        let model = self.cfg.lite_model().trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起轻量级模型请求"));
        }

        info!(
            model = %model,
            lite_model_config = %self.cfg.api_lite_model,
            timeout_ms = timeout_ms,
            prompt_length = prompt.len(),
            "开始调用轻量级模型"
        );

        let api_base = normalize_api_base(&self.cfg.api_base_url)?;

        let messages = vec![
            ChatCompletionRequestSystemMessageArgs::default()
                .content(
                    "你是会话标题生成助手。根据用户输入生成简洁的标题，要求：\
                    1. 标题不超过10个汉字\
                    2. 直接返回标题，不要任何解释或额外文字\
                    3. 标题要概括性强，简洁明了",
                )
                .build()
                .context("构建 system 消息失败")?
                .into(),
            ChatCompletionRequestUserMessageArgs::default()
                .content(prompt.to_string())
                .build()
                .context("构建 user 消息失败")?
                .into(),
        ];

        let mut request_args_binding = CreateChatCompletionRequestArgs::default();
        let request = request_args_binding
            .model(model.to_string())
            .messages(messages)
            .max_tokens(30u16)
            .temperature(0.3f32)
            .stream(false)
            .thinking(ChatThinking {
                r#type: Some(ChatThinkingType::Disabled),
                clear_thinking: None,
            })
            .build()
            .context("构建轻量级请求失败")?;

        let config = OpenAIConfig::new()
            .with_api_key(token.to_string())
            .with_api_base(api_base);
        let client = OpenAIClient::with_config(config);

        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;

        let response = runtime.block_on(async {
            timeout(
                Duration::from_millis(timeout_ms),
                client
                    .chat()
                    .create_byot::<_, CreateChatCompletionResponse>(request),
            )
            .await
        });

        let resp = match response {
            Ok(Ok(resp)) => resp,
            Ok(Err(err)) => {
                let hint = build_sdk_error_hint(&err.to_string());
                tracing::error!(
                    model = %model,
                    error = %err,
                    timeout_ms = %timeout_ms,
                    "轻量级模型请求失败",
                );
                return Err(anyhow!("轻量级模型请求失败：{err}{hint}"));
            }
            Err(_) => {
                tracing::error!(
                    model = %model,
                    timeout_ms = %timeout_ms,
                    "轻量级模型请求超时",
                );
                return Err(anyhow!("轻量级模型请求超时：{timeout_ms}ms"));
            }
        };

        let text = resp
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .unwrap_or("")
            .trim()
            .to_string();

        if text.is_empty() {
            warn!(
                model = %model,
                response = ?resp,
                "轻量级模型返回空响应",
            );
        } else {
            info!(
                model = %model,
                response_length = text.len(),
                response_preview = %text.chars().take(20).collect::<String>(),
                "轻量级模型返回成功",
            );
        }

        Ok(text)
    }

    /// 轻量级意图分类请求
    ///
    /// 与 `complete_lite` 类似，使用最小 system prompt，不注入 MCP 工具/工作目录等上下文，
    /// 使用 lite 模型、短超时、低 max_tokens，关闭 thinking。
    pub fn complete_classify(&self, system_prompt: &str, user_input: &str) -> Result<ModelResponse> {
        let token = self.cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空"));
        }

        let timeout_ms = 15_000u64;
        let model = self.cfg.lite_model().trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空"));
        }

        let api_base = normalize_api_base(&self.cfg.api_base_url)?;

        let messages = vec![
            ChatCompletionRequestSystemMessageArgs::default()
                .content(system_prompt.to_string())
                .build()
                .context("构建 system 消息失败")?
                .into(),
            ChatCompletionRequestUserMessageArgs::default()
                .content(user_input.to_string())
                .build()
                .context("构建 user 消息失败")?
                .into(),
        ];

        let mut request_args_binding = CreateChatCompletionRequestArgs::default();
        let request = request_args_binding
            .model(model.to_string())
            .messages(messages)
            .max_tokens(10u16)
            .temperature(0.0f32)
            .stream(false)
            .thinking(ChatThinking {
                r#type: Some(ChatThinkingType::Disabled),
                clear_thinking: None,
            })
            .build()
            .context("构建分类请求失败")?;

        let config = OpenAIConfig::new()
            .with_api_key(token.to_string())
            .with_api_base(api_base);
        let client = OpenAIClient::with_config(config);

        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;

        let response = runtime.block_on(async {
            timeout(
                Duration::from_millis(timeout_ms),
                client
                    .chat()
                    .create_byot::<_, CreateChatCompletionResponse>(request),
            )
            .await
        });

        let resp = match response {
            Ok(Ok(resp)) => resp,
            Ok(Err(err)) => return Err(anyhow!("意图分类请求失败：{err}")),
            Err(_) => return Err(anyhow!("意图分类请求超时：{timeout_ms}ms")),
        };

        let text = resp
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .unwrap_or("")
            .trim()
            .to_string();

        let usage = resp
            .usage
            .map(|u| TokenUsage {
                prompt_tokens: u.prompt_tokens as usize,
                completion_tokens: u.completion_tokens as usize,
                total_tokens: u.total_tokens as usize,
            })
            .unwrap_or_default();

        Ok(ModelResponse {
            text,
            reasoning_content: String::new(),
            usage,
            output_mode: "classify".to_string(),
            output_chunk_count: 0,
        })
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
        let mut request_args_binding = CreateChatCompletionRequestArgs::default();
        let mut request_args = request_args_binding
            .model(model.to_string())
            .messages(messages);
        if let Some(max_tokens) = configured_max_tokens() {
            request_args = request_args.max_tokens(max_tokens);
        }
        let mut request = request_args.build().context("构建 OpenAI 请求失败")?;
        request.stream = Some(false);
        let request_for_fallback = request.clone();

        let config = OpenAIConfig::new()
            .with_api_key(token.to_string())
            .with_api_base(api_base);
        let client = OpenAIClient::with_config(config);
        let mut request_json = serde_json::to_value(&request).context("序列化请求失败")?;
        inject_temperature_config(&mut request_json);
        inject_thinking_config(&mut request_json);

        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;

        let response = runtime.block_on(async {
            timeout(
                Duration::from_millis(timeout_ms),
                client.chat().create_byot::<_, Value>(request_json),
            )
            .await
        });

        if let Ok(Ok(payload)) = response
            && let Some(resp) = parse_non_stream_byot_response(&payload)
        {
            return Ok(resp);
        }

        let response = runtime.block_on(async {
            timeout(
                Duration::from_millis(timeout_ms),
                client.chat().create(request_for_fallback),
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
            reasoning_content: String::new(),
            usage: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            },
            output_mode: "non-stream".to_string(),
            output_chunk_count: 1,
        })
    }

    fn complete_with_functions(
        &self,
        req: &ModelRequest,
        functions: &[FunctionToolSpec],
    ) -> Result<ModelFunctionResponse> {
        let token = self.cfg.api_auth_token.trim();
        if token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空，无法发起工具模型请求"));
        }

        let timeout_ms = parse_function_timeout_ms(&self.cfg.api_timeout_ms)?;
        let model = self.cfg.api_model.trim();
        if model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空，无法发起工具模型请求"));
        }

        let api_base = normalize_api_base(&self.cfg.api_base_url)?;
        let messages = build_openai_messages(req)?;
        let mut request_args_binding = CreateChatCompletionRequestArgs::default();
        let request_args = request_args_binding
            .model(model.to_string())
            .messages(messages);
        let mut request = request_args
            .build()
            .context("构建 function call 请求失败")?;
        request.stream = Some(false);

        let config = OpenAIConfig::new()
            .with_api_key(token.to_string())
            .with_api_base(api_base);
        let client = OpenAIClient::with_config(config);
        let mut request_json = serde_json::to_value(&request).context("序列化请求失败")?;
        inject_temperature_config(&mut request_json);
        inject_thinking_config(&mut request_json);
        inject_function_tools(&mut request_json, functions);

        let runtime = TokioRuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .context("初始化异步运行时失败")?;

        let response = runtime.block_on(async {
            timeout(
                Duration::from_millis(timeout_ms),
                client.chat().create_byot::<_, Value>(request_json),
            )
            .await
        });

        match response {
            Ok(Ok(payload)) => parse_function_byot_response(&payload),
            Ok(Err(err)) => {
                let hint = build_sdk_error_hint(&err.to_string());
                Err(anyhow!("OpenAI SDK 工具调用请求失败：{err}{hint}"))
            }
            Err(_) => Err(anyhow!("工具调用请求超时：{timeout_ms}ms")),
        }
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

fn build_openai_messages(req: &ModelRequest) -> Result<Vec<ChatCompletionRequestMessage>> {
    let mut messages = Vec::new();
    let mut system_texts = vec![
        format!("当前会话：{}", req.session_title),
        format!("当前工作目录：{}", current_working_directory_text()),
        format!("允许文件操作目录：{}", allowed_file_roots_text()),
    ];
    if let Some(mcp_tools_prompt) = build_mcp_tools_system_prompt(24) {
        system_texts.push(mcp_tools_prompt);
    }

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
fn normalize_api_base(base_url: &str) -> Result<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("API_BASE_URL 不能为空"));
    }

    let cleaned = trimmed.trim_end_matches('/');
    let cleaned = cleaned.strip_suffix("/chat/completions").unwrap_or(cleaned);
    Ok(cleaned.to_string())
}

fn build_sdk_error_hint(error_text: &str) -> String {
    if error_text.contains("/chat/completions") && error_text.contains("404") {
        return "；请检查 API_BASE_URL 是否包含正确的版本路径（如 .../v1）".to_string();
    }
    if error_text.contains("expected struct ApiError") {
        return "；当前网关返回的错误结构非 OpenAI 标准格式，请确认 API_BASE_URL 是否为 OpenAI 兼容接口".to_string();
    }
    String::new()
}

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

fn should_skip_stream_payload(raw: &str) -> bool {
    let normalized = raw.trim().to_ascii_lowercase();
    normalized.is_empty()
        || normalized == "[done]"
        || normalized == "ping"
        || normalized == "pong"
        || normalized.contains("\"event\":\"ping\"")
}

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
fn inject_stream_usage_option(payload: &mut Value) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("stream_options".to_string(), json!({"include_usage": true}));
}

fn inject_temperature_config(payload: &mut Value) {
    let Some(temp) = configured_temperature_number() else {
        return;
    };
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    obj.insert("temperature".to_string(), Value::Number(temp));
}

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
