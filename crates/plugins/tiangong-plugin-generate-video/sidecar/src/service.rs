//! Generate-Video sidecar 业务服务。
//!
//! 解析视频生成端点 → 调供应商（含内部轮询）→ 返回状态结果。

use anyhow::{Context, Result};
use tiangong_llm::ModelCapability;
use tiangong_plugin_generate_video_protocol::{
    GENERATE_OPERATION, GenerateRequest, GenerateResponse, PLUGIN_ID, PLUGIN_VERSION,
    VIDEO_PROTOCOL_VERSION, VideoStatusWrapper,
};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};

pub struct VideoService;

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for VideoService {
    async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "Video 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
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
            business_protocol: VIDEO_PROTOCOL_VERSION,
            capabilities: vec!["video_generation".to_string()],
            instance_id: format!("video-sidecar-{}", std::process::id()),
            status: ServiceStatus::Ready,
        })
        .context("序列化 Video 握手响应失败"),

        GENERATE_OPERATION => {
            let req: GenerateRequest =
                serde_json::from_value(payload).context("解析 generate 请求失败")?;
            let result = generate(req).await?;
            serde_json::to_value(result).context("序列化 generate 响应失败")
        }

        other => Err(anyhow::anyhow!("未知的 Video 操作: {other}")),
    }
}

/// 生成视频：解析端点 → 调供应商（含轮询）→ 返回状态。
async fn generate(req: GenerateRequest) -> Result<GenerateResponse> {
    if req.prompt.trim().is_empty() {
        anyhow::bail!("prompt 不能为空");
    }

    let resolved =
        tiangong_plugin_sidecar::model::resolve_for_capability(ModelCapability::VideoGeneration)?;
    let output = tiangong_core::media::generate_video_with(
        &resolved,
        req.prompt,
        req.duration,
        req.resolution,
    )
    .await
    .map_err(|e| anyhow::anyhow!("视频生成供应商调用失败：{e}"))?;

    // 将 VideoGenStatus 四态映射到协议层包装结构。
    use tiangong_media::video::VideoGenStatus;
    let status = match output.response.status {
        VideoGenStatus::Completed {
            video_url,
            duration,
        } => VideoStatusWrapper {
            completed: true,
            video_url: Some(video_url),
            duration,
            ..Default::default()
        },
        VideoGenStatus::Pending => VideoStatusWrapper {
            pending: true,
            task_id: Some(output.response.task_id.clone()),
            ..Default::default()
        },
        VideoGenStatus::Processing { progress } => VideoStatusWrapper {
            processing: true,
            task_id: Some(output.response.task_id.clone()),
            progress,
            ..Default::default()
        },
        VideoGenStatus::Failed { error } => VideoStatusWrapper {
            failed: true,
            error: Some(error),
            ..Default::default()
        },
    };

    Ok(GenerateResponse {
        status,
        model: output.resolved.model,
    })
}
