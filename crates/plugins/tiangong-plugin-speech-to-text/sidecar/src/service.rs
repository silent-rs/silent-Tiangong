//! Speech-To-Text sidecar 业务服务。
//!
//! 校验音频路径 → 读取音频 → 解析 STT 端点 → 调供应商 → 返回转写文本。

use anyhow::{Context, Result};
use tiangong_llm::ModelCapability;
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};
use tiangong_plugin_runtime::sidecar::STORAGE_ROOT_ENV;
use tiangong_plugin_speech_to_text_protocol::{
    PLUGIN_ID, PLUGIN_VERSION, STT_PROTOCOL_VERSION, TRANSCRIBE_OPERATION, TranscribeRequest,
    TranscribeResponse,
};

pub struct SttService;

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for SttService {
    async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "STT 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
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
            business_protocol: STT_PROTOCOL_VERSION,
            capabilities: vec!["speech_to_text".to_string()],
            instance_id: format!("stt-sidecar-{}", std::process::id()),
            status: ServiceStatus::Ready,
        })
        .context("序列化 STT 握手响应失败"),

        TRANSCRIBE_OPERATION => {
            let req: TranscribeRequest =
                serde_json::from_value(payload).context("解析 transcribe 请求失败")?;
            let result = transcribe(req).await?;
            serde_json::to_value(result).context("序列化 transcribe 响应失败")
        }

        other => Err(anyhow::anyhow!("未知的 STT 操作: {other}")),
    }
}

/// 转写音频：校验路径 → 读取音频 → 调 STT 供应商 → 返回文本。
async fn transcribe(req: TranscribeRequest) -> Result<TranscribeResponse> {
    if req.file_path.trim().is_empty() {
        anyhow::bail!("file_path 不能为空");
    }

    // 安全限制：仅允许读取 ~/.tiangong/media/ 目录内的音频文件。
    let storage_root = std::env::var(STORAGE_ROOT_ENV)
        .context("TIANGONG_STORAGE_ROOT 未注入，无法定位媒体目录")?;
    let media_dir = std::path::PathBuf::from(&storage_root).join("media");
    let mime_type = match std::path::Path::new(&req.file_path)
        .extension()
        .and_then(|e| e.to_str())
    {
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") | Some("oga") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("webm") => "audio/webm",
        Some("m4a") => "audio/mp4",
        _ => anyhow::bail!("不支持的音频格式（仅支持 mp3/wav/ogg/flac/webm/m4a）"),
    };

    let canonical = std::fs::canonicalize(&req.file_path).context("文件不存在或无法访问")?;
    let canonical_media = std::fs::canonicalize(&media_dir).context("媒体目录不存在")?;
    if !canonical.starts_with(&canonical_media) {
        anyhow::bail!("音频文件必须在 ~/.tiangong/media 目录下");
    }

    let audio_data = std::fs::read(&canonical).context("读取音频文件失败")?;

    // 解析 STT 端点。
    let resolved = tiangong_plugin_sidecar::model::resolve_for_capability(ModelCapability::Stt)?;
    let output = tiangong_core::media::transcribe_audio_with(
        &resolved,
        audio_data,
        mime_type.to_string(),
        req.language,
    )
    .await
    .map_err(|e| anyhow::anyhow!("STT 供应商调用失败：{e}"))?;

    Ok(TranscribeResponse {
        text: output.response.text,
        language: output.response.language,
        duration: output.response.duration,
        model: output.resolved.model,
    })
}
