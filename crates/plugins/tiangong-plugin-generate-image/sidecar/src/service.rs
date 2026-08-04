//! Generate-Image sidecar 业务服务。
//!
//! 解析图片生成端点 → 调供应商 → 归档图片到 ~/.tiangong/media/images/ → 返回本地路径。

use anyhow::{Context, Result};
use tiangong_llm::ModelCapability;
use tiangong_plugin_generate_image_protocol::{
    Empty, GENERATE_OPERATION, GenerateRequest, GenerateResponse, GeneratedImage,
    IMAGE_PROTOCOL_VERSION, LIST_MODELS_OPERATION, ListModelsResponse, ModelInfo, PLUGIN_ID,
    PLUGIN_VERSION,
};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};

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
                    "Image 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
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

async fn dispatch_operation(
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    match operation {
        HANDSHAKE_OPERATION => serde_json::to_value(HandshakeResponse {
            plugin_id: PLUGIN_ID.to_string(),
            plugin_version: PLUGIN_VERSION.to_string(),
            sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            business_protocol: IMAGE_PROTOCOL_VERSION,
            capabilities: vec!["image_generation".to_string()],
            instance_id: format!("image-sidecar-{}", std::process::id()),
            status: ServiceStatus::Ready,
        })
        .context("序列化 Image 握手响应失败"),

        GENERATE_OPERATION => {
            let req: GenerateRequest =
                serde_json::from_value(payload).context("解析 generate 请求失败")?;
            let result = generate(req).await?;
            serde_json::to_value(result).context("序列化 generate 响应失败")
        }

        LIST_MODELS_OPERATION => {
            let _payload: Empty = serde_json::from_value(payload).unwrap_or_default();
            let response = list_models()?;
            serde_json::to_value(response).context("序列化 list_models 响应失败")
        }

        other => Err(anyhow::anyhow!("未知的 Image 操作: {other}")),
    }
}

/// 生成图片：解析端点 → 调供应商 → 归档 → 返回本地路径列表。
async fn generate(req: GenerateRequest) -> Result<GenerateResponse> {
    if req.prompt.trim().is_empty() {
        anyhow::bail!("prompt 不能为空");
    }

    let resolved =
        tiangong_plugin_sidecar::model::resolve_for_capability(ModelCapability::ImageGeneration)?;
    let output = tiangong_core::media::generate_image_with(
        &resolved,
        req.prompt,
        req.width.unwrap_or(0),
        req.height.unwrap_or(0),
        req.style,
    )
    .await
    .map_err(|e| anyhow::anyhow!("图片生成供应商调用失败：{e}"))?;

    // 每张图片归档到本地，失败则保留原始引用。
    let mut images = Vec::new();
    for img in &output.response.images {
        let raw = if let Some(url) = &img.url {
            url.clone()
        } else if let Some(b64) = &img.b64_data {
            format!("data:image/png;base64,{b64}")
        } else {
            continue;
        };
        let reference = match tiangong_media_archive::archive_image_reference(&raw, None, None) {
            Ok(archived) => archived.path().to_string(),
            Err(err) => {
                tracing::warn!(error = %err, "图片归档失败，保留原始引用");
                raw
            }
        };
        images.push(GeneratedImage { reference });
    }

    Ok(GenerateResponse {
        images,
        model: output.resolved.model,
    })
}

fn list_models() -> Result<ListModelsResponse> {
    let models = tiangong_plugin_sidecar::model::list_models_for_capability(
        ModelCapability::ImageGeneration,
    )?;
    Ok(ListModelsResponse {
        models: models
            .into_iter()
            .map(|m| ModelInfo {
                key: m.key,
                provider: m.provider,
                model: m.model,
                configured: m.configured,
            })
            .collect(),
    })
}
