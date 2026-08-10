//! Generate-Image-OpenAI sidecar 业务服务。
//!
//! 通过 OpenAI 兼容的 Chat Completions 接口生成图片，解析响应中的图片并归档落盘。
//! 支持两种模型来源：全局模型配置（models.json）或手动输入端点。

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tiangong_llm::{ModelCapability, ModelsConfig, ResolvedModel};
use tiangong_plugin_generate_image_openai_protocol::{
    Ack, ConfigBootstrap, ConfigSelection, Empty, GENERATE_OPERATION, GET_CONFIG_OPERATION,
    GenerateRequest, GenerateResponse, GeneratedImage, IMAGE_PROTOCOL_VERSION, ImageGenConfig,
    ManualEndpoint, ModelInfo, ModelSource, PLUGIN_ID, PLUGIN_VERSION, RECONFIGURE_OPERATION,
    SET_CONFIG_OPERATION,
};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};
use tiangong_plugin_sidecar::model::{resolve_for_capability, resolve_for_model_key};

use crate::config;
use crate::extract;

pub struct ImageService;

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for ImageService {
    async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                    request.protocol_version
                ),
                false,
            );
        }

        let payload = match dispatch_operation(&request.operation, request.payload).await {
            Ok(value) => value,
            Err(error) => {
                return Response::error(
                    &request_id,
                    ErrorCode::ServiceError,
                    error.to_string(),
                    false,
                );
            }
        };
        Response::success(&request_id, payload)
    }
}

async fn dispatch_operation(operation: &str, payload: Value) -> Result<Value> {
    match operation {
        HANDSHAKE_OPERATION => serde_json::to_value(HandshakeResponse {
            plugin_id: PLUGIN_ID.to_string(),
            plugin_version: PLUGIN_VERSION.to_string(),
            sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            business_protocol: IMAGE_PROTOCOL_VERSION,
            capabilities: vec!["image_generation".to_string()],
            instance_id: format!("image-openai-sidecar-{}", std::process::id()),
            status: ServiceStatus::Ready,
        })
        .context("序列化握手响应失败"),

        GENERATE_OPERATION => {
            let req: GenerateRequest =
                serde_json::from_value(payload).context("解析 generate 请求失败")?;
            let result = generate(req).await?;
            serde_json::to_value(result).context("序列化 generate 响应失败")
        }

        GET_CONFIG_OPERATION => {
            let _payload: Empty = serde_json::from_value(payload).unwrap_or_default();
            let bootstrap = build_bootstrap()?;
            serde_json::to_value(bootstrap).context("序列化配置 bootstrap 响应失败")
        }

        SET_CONFIG_OPERATION => {
            let selection: ConfigSelection =
                serde_json::from_value(payload).context("解析配置选择失败")?;
            config::save_selection(&selection)?;
            serde_json::to_value(Ack::default()).context("序列化配置保存响应失败")
        }

        RECONFIGURE_OPERATION => {
            // 重新读盘：on_config_updated 触发后确认最新配置。
            let _ = config::load_or_default();
            serde_json::to_value(Ack::default()).context("序列化 reconfigure 响应失败")
        }

        other => Err(anyhow!("未知的操作: {other}")),
    }
}

/// 生成图片：读配置 → 解析端点 → 调 Chat Completions → 提取图片 → 归档落盘。
async fn generate(req: GenerateRequest) -> Result<GenerateResponse> {
    if req.prompt.trim().is_empty() {
        anyhow::bail!("prompt 不能为空");
    }

    let config = config::load_or_default();
    let resolved = resolve_endpoint(&config)?;
    let payload = build_chat_request(&req.prompt, &resolved.model, &config);

    let response = call_chat_completions(&resolved, payload).await?;
    let raw_images = extract::extract_images(&response)?;
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&resolved.model)
        .to_string();

    let mut images = Vec::new();
    for raw in &raw_images {
        let reference = match tiangong_media_archive::archive_image_reference(raw, None, None) {
            Ok(archived) => archived.path().to_string(),
            Err(err) => {
                tracing::warn!(error = %err, "图片归档失败，保留原始引用");
                raw.clone()
            }
        };
        images.push(GeneratedImage { reference });
    }

    Ok(GenerateResponse { images, model })
}

/// 根据配置解析模型端点。
fn resolve_endpoint(config: &ImageGenConfig) -> Result<ResolvedModel> {
    match config.source {
        ModelSource::Global => {
            // 优先按用户选择的 key 解析，未指定时回退到 chat 能力路由。
            if let Some(key) = config.global_model_key.as_deref() {
                return resolve_for_model_key(key);
            }
            resolve_for_capability(ModelCapability::Chat)
        }
        ModelSource::Manual => resolve_manual(&config.manual_endpoint),
    }
}

/// 解析手动输入的端点。
fn resolve_manual(endpoint: &ManualEndpoint) -> Result<ResolvedModel> {
    if endpoint.base_url.trim().is_empty() {
        anyhow::bail!("手动端点缺少 base_url");
    }
    if endpoint.model.trim().is_empty() {
        anyhow::bail!("手动端点缺少 model id");
    }
    // 支持 ${ENV_VAR} 形式的环境变量引用。
    let api_key = ModelsConfig::resolve_api_key(&endpoint.api_key);
    if api_key.trim().is_empty() {
        anyhow::bail!("手动端点缺少 api_key");
    }
    Ok(ResolvedModel {
        provider: "manual".to_string(),
        base_url: endpoint.base_url.clone(),
        api_key,
        timeout_ms: 120_000,
        protocol: tiangong_llm::ProviderProtocol::OpenAiChatCompletions,
        model: endpoint.model.clone(),
        options: Value::Null,
        context_window: None,
    })
}

/// 构造 Chat Completions 请求体。
fn build_chat_request(prompt: &str, model: &str, config: &ImageGenConfig) -> Value {
    let mut messages = Vec::new();
    // system 消息（可选附加提示）。
    if let Some(extra) = config.extra_prompt.as_deref() {
        let trimmed = extra.trim();
        if !trimmed.is_empty() {
            messages.push(json!({"role": "system", "content": trimmed}));
        }
    }
    messages.push(json!({"role": "user", "content": prompt}));

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });

    if config.enable_modalities {
        body["modalities"] = json!(["text", "image"]);
    }

    body
}

/// 调用 Chat Completions 接口，返回原始响应 JSON。
async fn call_chat_completions(resolved: &ResolvedModel, payload: Value) -> Result<Value> {
    let base = resolved.base_url.trim_end_matches('/');
    // 兼容用户填的 base_url 末尾是否带 /chat/completions。
    let url = if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("构造 HTTP 客户端失败")?;

    tracing::debug!(url = %url, model = %resolved.model, "Chat Completions 生图请求");

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", resolved.api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .context("请求 Chat Completions 接口失败")?;

    let status = resp.status();
    let resp_text = resp.text().await.context("读取响应体失败")?;

    if !status.is_success() {
        let err_msg = extract_error_message(&resp_text).unwrap_or(resp_text.clone());
        anyhow::bail!("Chat Completions 调用失败 ({status}): {err_msg}");
    }

    serde_json::from_str::<Value>(&resp_text)
        .with_context(|| format!("解析 Chat Completions 响应失败：{resp_text}"))
}

/// 从错误响应里提取可读的 message 字段。
fn extract_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// 构造设置页 bootstrap：当前配置 + 全局可选模型列表。
fn build_bootstrap() -> Result<ConfigBootstrap> {
    let config = config::load_or_default();
    let models = tiangong_plugin_sidecar::model::list_models_for_capability(ModelCapability::Chat)?;
    let model_infos = models
        .into_iter()
        .map(|m| ModelInfo {
            key: m.key,
            provider: m.provider,
            model: m.model,
            configured: m.configured,
        })
        .collect();
    Ok(ConfigBootstrap {
        config,
        models: model_infos,
    })
}
