//! Analyze-Attachment sidecar 业务服务。
//!
//! 读取图片文件 → 构造多模态 ModelRequest → 调 SingleProviderClient → 返回分析文本。

use anyhow::{Context, Result};
use tiangong_core::session::{Message, MessageRole};
use tiangong_llm::{ModelCapability, ModelEndpoint, ModelRequest, SingleProviderClient};
use tiangong_plugin_analyze_attachment_protocol::{
    ANALYZE_OPERATION, ATTACHMENT_PROTOCOL_VERSION, AnalyzeRequest, AnalyzeResponse, PLUGIN_ID,
    PLUGIN_VERSION,
};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};
use tiangong_types::{ContentBlock, MediaKind, StoredAsset};

pub struct AttachmentService;

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for AttachmentService {
    async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "Attachment 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
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
            business_protocol: ATTACHMENT_PROTOCOL_VERSION,
            capabilities: vec!["multimodal".to_string()],
            instance_id: format!("attachment-sidecar-{}", std::process::id()),
            status: ServiceStatus::Ready,
        })
        .context("序列化 Attachment 握手响应失败"),

        ANALYZE_OPERATION => {
            let req: AnalyzeRequest =
                serde_json::from_value(payload).context("解析 analyze 请求失败")?;
            let result = analyze(req).await?;
            serde_json::to_value(result).context("序列化 analyze 响应失败")
        }

        other => Err(anyhow::anyhow!("未知的 Attachment 操作: {other}")),
    }
}

/// 分析附件：读图片 → 构造多模态请求 → 调模型 → 返回分析文本。
async fn analyze(req: AnalyzeRequest) -> Result<AnalyzeResponse> {
    if req.images.is_empty() {
        anyhow::bail!("没有可分析的图片");
    }

    // 解析 multimodal 端点。
    let models = tiangong_plugin_sidecar::model::load_models_config()?;
    let resolved = if models.chat_is_multimodal() {
        models
            .resolve_for_capability(ModelCapability::Chat)
            .ok_or_else(|| anyhow::anyhow!("Chat 模型未配置"))?
    } else {
        models
            .resolve_for_capability(ModelCapability::Multimodal)
            .ok_or_else(|| anyhow::anyhow!("Multimodal 能力未配置"))?
    };
    let endpoint = ModelEndpoint::from_resolved(resolved);
    let client = SingleProviderClient::new(endpoint);

    // 构造多模态请求上下文。
    let instruction = if req.instruction.trim().is_empty() {
        "请解析附件内容，并提取与用户问题有关的信息。".to_string()
    } else {
        req.instruction
    };

    let mut context = vec![
        Message::new(
            MessageRole::System,
            "你是附件解析助手。只根据随消息提供的附件内容和解析要求回答，输出可供主模型直接使用的简洁中文结果。".to_string(),
        ),
        Message::new(
            MessageRole::Assistant,
            "好的，我将作为附件解析助手，根据附件内容和解析要求进行分析。".to_string(),
        ),
    ];

    let mut user_message = Message::new(
        MessageRole::User,
        format!(
            "用户原始消息：{}\n\n解析要求：{}",
            req.user_message_text.trim(),
            instruction
        ),
    );

    // 读取每张图片，构造 ContentBlock::Image。
    for image_path in &req.images {
        let asset = asset_from_path(image_path)?;
        user_message
            .content
            .push(ContentBlock::Image { asset, data: None });
    }
    context.push(user_message);

    let request = ModelRequest {
        user_input: String::new(),
        context,
        reasoning_effort: tiangong_llm::request::ReasoningEffort::None,
        max_output_tokens: None,
    };

    let response = client
        .complete_async(&request)
        .await
        .map_err(|e| anyhow::anyhow!("多模态模型调用失败：{e}"))?;

    Ok(AnalyzeResponse {
        text: response.text,
        prompt_tokens: response.usage.prompt_tokens as u64,
        completion_tokens: response.usage.completion_tokens as u64,
        model: String::new(),
    })
}

/// 从图片路径构造 StoredAsset（读取文件元信息推断 MIME）。
fn asset_from_path(path: &str) -> Result<StoredAsset> {
    let path_trimmed = path.trim();
    if path_trimmed.is_empty() {
        anyhow::bail!("图片路径为空");
    }
    let mime_type = infer_image_mime(path_trimmed);
    let size = std::fs::metadata(path_trimmed)
        .map(|m| m.len())
        .unwrap_or(0);
    let original_name = std::path::Path::new(path_trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();
    Ok(StoredAsset {
        asset_id: format!("attachment-{}", scru128::new()),
        local_path: path_trimmed.to_string(),
        original_name,
        mime_type,
        size,
        kind: MediaKind::Image,
    })
}

fn infer_image_mime(path: &str) -> String {
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
    .to_string()
}
