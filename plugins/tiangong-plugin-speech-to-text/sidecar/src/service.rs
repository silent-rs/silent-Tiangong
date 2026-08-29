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
    Empty, PLUGIN_ID, PLUGIN_VERSION, RECORD_CANCEL_OPERATION, RECORD_START_OPERATION,
    RECORD_STOP_OPERATION, RecordControlRequest, RecordStartRequest, RecordStartResponse,
    RecordStopResponse, STT_PROTOCOL_VERSION, TRANSCRIBE_OPERATION, TranscribeRequest,
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

        RECORD_START_OPERATION => {
            let req: RecordStartRequest =
                serde_json::from_value(payload).context("解析 record_start 请求失败")?;
            let result = record_start(req)?;
            serde_json::to_value(result).context("序列化 record_start 响应失败")
        }

        RECORD_STOP_OPERATION => {
            let req: RecordControlRequest =
                serde_json::from_value(payload).context("解析 record_stop 请求失败")?;
            let result = record_stop(req)?;
            serde_json::to_value(result).context("序列化 record_stop 响应失败")
        }

        RECORD_CANCEL_OPERATION => {
            let req: RecordControlRequest =
                serde_json::from_value(payload).context("解析 record_cancel 请求失败")?;
            record_cancel(req)?;
            serde_json::to_value(Empty {}).context("序列化 record_cancel 响应失败")
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
        audio_path: req.file_path,
    })
}

// ── 录音（record_start / record_stop / record_cancel）──
//
// 原生采集（cpal 默认麦克风 → 线性重采样 16 kHz → hound WAV，实现见
// record_session.rs），不依赖 ffmpeg/arecord 等外部命令。录音会话由全局
// 静态变量管理（sidecar 单进程、同一时刻至多一路录音）；停止/取消请求
// 必须携带 record_start 传入的会话 ID，迟到的旧请求不会误杀新录音。

use crate::record_session::{self, RecordSession};
use std::sync::Mutex;

static RECORDING: Mutex<Option<RecordSession>> = Mutex::new(None);

/// 开始录音：使用调用方传入的会话 ID 启动原生采集。
fn record_start(req: RecordStartRequest) -> Result<RecordStartResponse> {
    let session_id = req.session_id.trim().to_string();
    if session_id.is_empty() {
        anyhow::bail!("record_start 缺少 session_id（由调用方生成并传入）");
    }

    let mut guard = RECORDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 槽位残留只可能来自异常路径（前端崩溃未发取消）：悄悄取消旧会话
    // 并开始新录音，自愈优于报错卡死；正常流程前端不会在录音中再次开始。
    if let Some(stale) = guard.take() {
        stale.cancel();
    }

    let file_path = media_file_path("stt_rec", "wav")?;
    let session = record_session::start(session_id.clone(), file_path)?;
    *guard = Some(session);
    Ok(RecordStartResponse { session_id })
}

/// 停止录音：会话匹配校验 → 停流收尾 → 返回音频文件路径与时长。
fn record_stop(req: RecordControlRequest) -> Result<RecordStopResponse> {
    let mut guard = RECORDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = take_matching_session(&mut guard, &req)?;
    let file_path = session.file_path.to_string_lossy().to_string();
    let duration = session.stop()?;
    Ok(RecordStopResponse {
        file_path,
        mime_type: "audio/wav".to_string(),
        duration: Some(duration),
    })
}

/// 取消录音：会话匹配校验 → 终止采集并删除录音文件（用户放弃，不保留产物）。
///
/// 没有录音或会话不匹配时静默成功：取消是幂等操作，迟到的旧取消请求
/// 不应向用户报错。
fn record_cancel(req: RecordControlRequest) -> Result<()> {
    let mut guard = RECORDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(session) = take_matching_session(&mut guard, &req) {
        session.cancel();
    }
    Ok(())
}

/// 取出与请求会话 ID 匹配的录音会话；不匹配时**不动现有录音**并返回错误。
///
/// 防竞态核心：迟到的旧取消/停止请求（session_id 是旧录音的）不能终止
/// 已经开始的新录音——会话 ID 不匹配直接报错，RECORDING 槽位保持原样。
fn take_matching_session(
    guard: &mut Option<RecordSession>,
    req: &RecordControlRequest,
) -> Result<RecordSession> {
    let session = guard
        .take()
        .ok_or_else(|| anyhow::anyhow!("当前没有录音在进行中"))?;
    if session.session_id != req.session_id {
        // 放回新录音，拒绝旧会话的请求。
        *guard = Some(session);
        anyhow::bail!(
            "录音会话不匹配（请求 {}，当前为其他会话），已忽略",
            req.session_id
        );
    }
    Ok(session)
}

/// 构造 `~/.tiangong/media/<prefix>_<scru128>.<ext>` 路径并确保目录存在。
fn media_file_path(prefix: &str, ext: &str) -> Result<std::path::PathBuf> {
    let storage_root = std::env::var(STORAGE_ROOT_ENV)
        .context("TIANGONG_STORAGE_ROOT 未注入，无法定位媒体目录")?;
    let dir = std::path::PathBuf::from(&storage_root).join("media");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建媒体目录失败：{}", dir.display()))?;
    let file_name = format!("{}_{}.{}", prefix, scru128::new(), ext);
    Ok(dir.join(file_name))
}
