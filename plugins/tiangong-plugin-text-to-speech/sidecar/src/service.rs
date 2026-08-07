//! Text-To-Speech sidecar 业务服务。
//!
//! 从天工 models.json 解析 TTS 端点，调用供应商接口合成语音，音频落盘。

use anyhow::{Context, Result};
use tiangong_llm::ModelCapability;
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};
use tiangong_plugin_runtime::sidecar::STORAGE_ROOT_ENV;
use tiangong_plugin_text_to_speech_protocol::{
    Empty, LIST_MODELS_OPERATION, ListModelsResponse, ModelInfo, PLUGIN_ID, PLUGIN_VERSION,
    SYNTHESIZE_OPERATION, SynthesizeRequest, SynthesizeResponse, TTS_PROTOCOL_VERSION,
};

/// TTS sidecar 业务服务（无状态）。
pub struct TtsService;

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for TtsService {
    async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "TTS 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
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
            business_protocol: TTS_PROTOCOL_VERSION,
            capabilities: vec!["text_to_speech".to_string()],
            instance_id: format!("tts-sidecar-{}", std::process::id()),
            status: ServiceStatus::Ready,
        })
        .context("序列化 TTS 握手响应失败"),

        SYNTHESIZE_OPERATION => {
            let req: SynthesizeRequest =
                serde_json::from_value(payload).context("解析 synthesize 请求失败")?;
            let result = synthesize(req).await?;
            serde_json::to_value(result).context("序列化 synthesize 响应失败")
        }

        LIST_MODELS_OPERATION => {
            let _payload: Empty = serde_json::from_value(payload).unwrap_or_default();
            let response = list_models()?;
            serde_json::to_value(response).context("序列化 list_models 响应失败")
        }

        other => Err(anyhow::anyhow!("未知的 TTS 操作: {other}")),
    }
}

/// 合成语音：解析 TTS 端点 → 调供应商 → 音频落盘 → 返回文件路径。
async fn synthesize(req: SynthesizeRequest) -> Result<SynthesizeResponse> {
    if req.text.trim().is_empty() {
        anyhow::bail!("text 不能为空");
    }

    let resolved = tiangong_plugin_sidecar::model::resolve_for_capability(ModelCapability::Tts)?;
    let output = tiangong_core::media::synthesize_speech_with(
        &resolved, req.text, req.voice, req.speed, None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("TTS 供应商调用失败：{e}"))?;

    // 按返回的 mime_type 决定扩展名。
    let ext = match output.response.mime_type.as_str() {
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/opus" => "opus",
        "audio/aac" => "aac",
        "audio/flac" => "flac",
        _ => "mp3",
    };

    let file_path = media_file_path("tts", ext)?;
    std::fs::write(&file_path, &output.response.audio)
        .with_context(|| format!("写入音频文件失败：{}", file_path.display()))?;

    Ok(SynthesizeResponse {
        file_path: file_path.display().to_string(),
        mime_type: output.response.mime_type,
        duration: output.response.duration,
        model: output.resolved.model,
    })
}

/// 返回匹配 TTS 能力的脱敏模型列表。
fn list_models() -> Result<ListModelsResponse> {
    let models = tiangong_plugin_sidecar::model::list_models_for_capability(ModelCapability::Tts)?;
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

/// 构造 `~/.tiangong/media/tts_<scru128>.<ext>` 路径并确保目录存在。
fn media_file_path(prefix: &str, ext: &str) -> Result<std::path::PathBuf> {
    let storage_root = std::env::var(STORAGE_ROOT_ENV)
        .context("TIANGONG_STORAGE_ROOT 未注入，无法定位媒体目录")?;
    let dir = std::path::PathBuf::from(storage_root).join("media");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建媒体目录失败：{}", dir.display()))?;
    let file_name = format!("{}_{}.{}", prefix, scru128::new(), ext);
    Ok(dir.join(file_name))
}
