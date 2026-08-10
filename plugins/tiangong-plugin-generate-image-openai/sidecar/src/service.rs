//! Generate-Image-OpenAI sidecar 业务服务。
//!
//! 通过 OpenAI Responses API 的 image_generation 工具生成图片，解析响应并归档落盘。
//! 支持两种模型来源：全局模型配置（models.json）或手动输入端点。

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use serde_json::{Value, json};
use tiangong_llm::{ModelCapability, ModelsConfig, ResolvedModel};
use tiangong_plugin_generate_image_openai_protocol::{
    Ack, ConfigBootstrap, ConfigSelection, Empty, GENERATE_OPERATION, GET_CONFIG_OPERATION,
    GenerateRequest, GenerateResponse, GeneratedImage, IMAGE_PROTOCOL_VERSION, ImageGenConfig,
    ModelInfo, ModelSource, PLUGIN_ID, PLUGIN_VERSION, RECONFIGURE_OPERATION, ResolvedEndpoint,
    SET_CONFIG_OPERATION,
};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};

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
            // 保存时立即解析端点并缓存，运行时不再依赖 models.json。
            let resolved = resolve_selection_endpoint(&selection)?;
            config::save_selection(&selection, resolved)?;
            serde_json::to_value(Ack::default()).context("序列化配置保存响应失败")
        }

        RECONFIGURE_OPERATION => {
            // on_config_updated 触发：重新读盘并尝试刷新已缓存的端点。
            let existing = config::load_or_default();
            if let Some(updated) = try_refresh_resolved(&existing)? {
                config::save_resolved(&updated)?;
            }
            serde_json::to_value(Ack::default()).context("序列化 reconfigure 响应失败")
        }

        other => Err(anyhow!("未知的操作: {other}")),
    }
}

/// 生成/编辑图片：读配置 → 解析端点 → 调 Responses API → 提取图片 → 归档落盘。
///
/// `req.images` 非空时为编辑模式，空时为生成模式。
async fn generate(req: GenerateRequest) -> Result<GenerateResponse> {
    if req.prompt.trim().is_empty() {
        anyhow::bail!("prompt 不能为空");
    }

    let config = config::load_or_default();
    let resolved = resolve_endpoint(&config)?;
    let payload = build_responses_request(&req.prompt, &resolved.model, &config, &req.images)?;

    let response = call_responses_api(&resolved, payload).await?;
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

/// 运行时解析模型端点：只读 config.json 里缓存的 resolved，不再依赖 models.json。
///
/// resolved 在保存配置时（SET_CONFIG_OPERATION）或 on_config_updated 触发时已写入。
fn resolve_endpoint(config: &ImageGenConfig) -> Result<ResolvedModel> {
    let resolved = &config.resolved;
    if resolved.base_url.trim().is_empty() {
        anyhow::bail!("未配置有效端点，请在设置页选择模型或手动输入端点后保存");
    }
    if resolved.model.trim().is_empty() {
        anyhow::bail!("已缓存端点缺少 model");
    }
    // api_key 支持 ${ENV_VAR}，在保存配置时已解析；这里兜底再解析一次（兼容旧配置）。
    let api_key = ModelsConfig::resolve_api_key(&resolved.api_key);
    if api_key.trim().is_empty() {
        anyhow::bail!("已缓存端点缺少 api_key");
    }
    Ok(ResolvedModel {
        provider: config.source.key().to_string(),
        base_url: resolved.base_url.clone(),
        api_key,
        timeout_ms: 120_000,
        protocol: tiangong_llm::ProviderProtocol::OpenAiChatCompletions,
        model: resolved.model.clone(),
        options: Value::Null,
        context_window: None,
    })
}

/// 保存配置时解析选择对应的端点，写入 config.resolved 缓存。
///
/// - global：从 models.json 按 key（或 chat 能力回退）解析完整端点。
/// - manual：直接用手动输入的 base_url/api_key/model（api_key 解析 ${ENV_VAR}）。
///
/// 解析失败时返回错误（不保存），让 UI 提示用户。
fn resolve_selection_endpoint(selection: &ConfigSelection) -> Result<ResolvedEndpoint> {
    match selection.source {
        ModelSource::Global => {
            let resolved = if let Some(key) = selection.global_model_key.as_deref() {
                tiangong_plugin_sidecar::model::resolve_for_model_key(key)
            } else {
                tiangong_plugin_sidecar::model::resolve_for_capability(ModelCapability::Chat)
            }?;
            Ok(ResolvedEndpoint {
                base_url: resolved.base_url,
                api_key: resolved.api_key,
                model: resolved.model,
            })
        }
        ModelSource::Manual => {
            let endpoint = &selection.manual_endpoint;
            if endpoint.base_url.trim().is_empty() {
                anyhow::bail!("手动端点缺少 base_url");
            }
            if endpoint.model.trim().is_empty() {
                anyhow::bail!("手动端点缺少 model id");
            }
            let api_key = ModelsConfig::resolve_api_key(&endpoint.api_key);
            if api_key.trim().is_empty() {
                anyhow::bail!("手动端点缺少 api_key");
            }
            Ok(ResolvedEndpoint {
                base_url: endpoint.base_url.clone(),
                api_key,
                model: endpoint.model.clone(),
            })
        }
    }
}

/// on_config_updated 触发时尝试刷新已缓存的端点。
///
/// 仅 global 来源重新解析（用户可能改了 models.json）；manual 来源保持不变。
/// 解析失败时返回 None（保留旧配置，不阻断服务）。
fn try_refresh_resolved(existing: &ImageGenConfig) -> Result<Option<ImageGenConfig>> {
    if existing.source != ModelSource::Global {
        return Ok(None);
    }
    let selection = ConfigSelection::from(existing);
    match resolve_selection_endpoint(&selection) {
        Ok(new_resolved) => {
            if new_resolved == existing.resolved {
                Ok(None)
            } else {
                let mut updated = existing.clone();
                updated.resolved = new_resolved;
                Ok(Some(updated))
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "刷新全局端点失败，保留旧配置");
            Ok(None)
        }
    }
}

/// 构造 Responses API 请求体。
///
/// 使用 OpenAI Responses API 的内置 `image_generation` 工具。
/// - 无图片时为生成模式（input 是字符串）。
/// - 有图片时为编辑模式（input 是数组，含 input_text + 每张图的 input_image，tools 设 action: edit）。
fn build_responses_request(
    prompt: &str,
    model: &str,
    config: &ImageGenConfig,
    images: &[String],
) -> Result<Value> {
    let mut body = if images.is_empty() {
        // 生成模式
        json!({
            "model": model,
            "tools": [{"type": "image_generation"}],
            "input": prompt,
        })
    } else {
        // 编辑模式：input 数组含文本 + 每张原图
        let mut content = vec![json!({"type": "input_text", "text": prompt})];
        for path in images {
            let data_uri = read_image_as_data_uri(path)?;
            content.push(json!({"type": "input_image", "image_url": data_uri}));
        }
        json!({
            "model": model,
            "tools": [{"type": "image_generation", "action": "edit"}],
            "input": [{"role": "user", "content": content}],
        })
    };

    if let Some(extra) = config.extra_prompt.as_deref() {
        let trimmed = extra.trim();
        if !trimmed.is_empty() {
            body["instructions"] = json!(trimmed);
        }
    }
    Ok(body)
}

/// 读取本地图片文件，编码为 `data:image/{mime};base64,...` 形式。
fn read_image_as_data_uri(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        anyhow::bail!("图片路径为空");
    }
    let bytes = std::fs::read(trimmed).with_context(|| format!("读取图片文件失败：{trimmed}"))?;
    let mime = infer_image_mime(trimmed);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

/// 按扩展名推断图片 MIME。
fn infer_image_mime(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else {
        "image/png"
    }
}

/// 调用 Responses API（`POST {base_url}/responses`），返回原始响应 JSON。
async fn call_responses_api(resolved: &ResolvedModel, payload: Value) -> Result<Value> {
    let base = resolved.base_url.trim_end_matches('/');
    // 兼容用户填的 base_url 末尾是否带 /responses。
    let url = if base.ends_with("/responses") {
        base.to_string()
    } else {
        format!("{base}/responses")
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("构造 HTTP 客户端失败")?;

    tracing::debug!(url = %url, model = %resolved.model, "Responses API 生图请求");

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", resolved.api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .context("请求 Responses API 失败")?;

    let status = resp.status();
    let resp_text = resp.text().await.context("读取响应体失败")?;

    if !status.is_success() {
        let err_msg = extract_error_message(&resp_text).unwrap_or(resp_text.clone());
        anyhow::bail!("Responses API 调用失败 ({status}): {err_msg}");
    }

    serde_json::from_str::<Value>(&resp_text)
        .with_context(|| format!("解析 Responses API 响应失败：{resp_text}"))
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
