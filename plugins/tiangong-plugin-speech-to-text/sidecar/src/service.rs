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
    Empty, PLUGIN_ID, PLUGIN_VERSION, RECORD_START_OPERATION, RECORD_STOP_OPERATION,
    RecordStartRequest, RecordStartResponse, RecordStopResponse, STT_PROTOCOL_VERSION,
    TRANSCRIBE_OPERATION, TranscribeRequest, TranscribeResponse,
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
            let _payload: Empty = serde_json::from_value(payload).unwrap_or_default();
            let result = record_stop()?;
            serde_json::to_value(result).context("序列化 record_stop 响应失败")
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

// ── 录音（record_start / record_stop）──
//
// 用系统录音命令采集麦克风。录音进程由全局静态变量管理（sidecar 单进程）。
// 跨平台：
// - macOS：ffmpeg -f avfoundation -i ":0"（需安装 ffmpeg）
// - Linux：arecord（ALSA 默认）
// - Windows：powershell 录音（复杂，暂用 ffmpeg）

use std::sync::Mutex;

static RECORDING: Mutex<Option<std::process::Child>> = Mutex::new(None);

/// 开始录音：启动系统录音命令，返回会话 ID。
fn record_start(req: RecordStartRequest) -> Result<RecordStartResponse> {
    let mut guard = RECORDING.lock().map_err(|_| anyhow::anyhow!("录音锁获取失败"))?;
    if guard.is_some() {
        anyhow::bail!("已有录音在进行中");
    }

    let sample_rate = req.sample_rate.unwrap_or(16000);
    let file_path = media_file_path("stt_rec", "wav")?;

    #[cfg(target_os = "macos")]
    let child = {
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-f", "avfoundation",
            "-i", ":0",
            "-ar", &sample_rate.to_string(),
            "-ac", "1",
            file_path.to_string_lossy().as_ref(),
        ]);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("启动录音失败（需安装 ffmpeg）：{e}"))?
    };

    #[cfg(target_os = "linux")]
    let child = {
        let mut cmd = std::process::Command::new("arecord");
        cmd.args([
            "-f", "S16_LE",
            "-r", &sample_rate.to_string(),
            "-c", "1",
            &file_path.to_string_lossy().to_string(),
        ]);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("启动录音失败（需安装 arecord）：{e}"))?
    };

    #[cfg(target_os = "windows")]
    let child = {
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-f", "dshow",
            "-i", "audio=default",
            "-ar", &sample_rate.to_string(),
            "-ac", "1",
            &file_path.to_string_lossy().to_string(),
        ]);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("启动录音失败（需安装 ffmpeg）：{e}"))?
    };

    let session_id = format!("rec-{}", scru128::new());
    *guard = Some(child);
    Ok(RecordStartResponse { session_id })
}

/// 停止录音：终止录音进程，返回音频文件路径。
fn record_stop() -> Result<RecordStopResponse> {
    let mut guard = RECORDING.lock().map_err(|_| anyhow::anyhow!("录音锁获取失败"))?;
    let child = guard.take().ok_or_else(|| anyhow::anyhow!("当前没有录音在进行中"))?;

    // 终止录音进程。
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();

    // 录音文件路径：record_start 时生成，这里需要恢复。
    // 简化：从 media 目录找最新的 stt_rec_*.wav。
    let storage_root = std::env::var(STORAGE_ROOT_ENV)
        .context("TIANGONG_STORAGE_ROOT 未注入，无法定位媒体目录")?;
    let media_dir = std::path::PathBuf::from(&storage_root).join("media");
    let latest = std::fs::read_dir(&media_dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("stt_rec_") && n.ends_with(".wav"))
                })
                .max_by_key(|p| p.metadata().map(|m| m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH)).unwrap_or(std::time::SystemTime::UNIX_EPOCH))
        });

    let file_path = latest.ok_or_else(|| anyhow::anyhow!("未找到录音文件"))?;
    let duration = file_path
        .metadata()
        .ok()
        .map(|m| m.len() as f64 / (16000.0 * 2.0));

    Ok(RecordStopResponse {
        file_path: file_path.to_string_lossy().to_string(),
        mime_type: "audio/wav".to_string(),
        duration,
    })
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
