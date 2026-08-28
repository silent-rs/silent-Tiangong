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
    Empty, LIST_MODELS_OPERATION, LIST_VOICES_OPERATION, ListModelsResponse, ListVoicesResponse,
    ModelInfo, PLAY_OPERATION, PLAY_STATUS_OPERATION, PLUGIN_ID, PLUGIN_VERSION, PlayRequest,
    PlayResponse, PlayStatusResponse, STOP_OPERATION, SYNTHESIZE_OPERATION, SynthesizeRequest,
    SynthesizeResponse, TTS_PROTOCOL_VERSION, VoiceInfo,
};

/// Windows 下抑制子进程控制台窗口（CREATE_NO_WINDOW），避免播放时闪黑窗。
#[cfg(windows)]
fn suppress_console_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn suppress_console_window(_command: &mut std::process::Command) {}

/// 后台播放进程状态（sidecar 单进程，全局唯一一路播放）。
struct PlayState {
    child: std::process::Child,
}

static PLAYING: std::sync::Mutex<Option<PlayState>> = std::sync::Mutex::new(None);

/// 收割已退出的播放进程；仍在播放时返回 `true`。
///
/// 锁中毒时按「无播放」处理，避免播放状态异常卡死 stop/play。
fn play_running() -> bool {
    let Ok(mut guard) = PLAYING.lock() else {
        return false;
    };
    match guard.as_mut() {
        None => false,
        Some(state) => match state.child.try_wait() {
            Ok(Some(_)) | Err(_) => {
                *guard = None;
                false
            }
            Ok(None) => true,
        },
    }
}

/// 终止当前播放进程（仅终止本 sidecar 自己启动的进程，不按进程名全局匹配）。
fn stop_playback() {
    let Ok(mut guard) = PLAYING.lock() else {
        return;
    };
    if let Some(mut state) = guard.take() {
        let _ = state.child.kill();
        let _ = state.child.wait();
    }
}

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

        LIST_VOICES_OPERATION => {
            let _payload: Empty = serde_json::from_value(payload).unwrap_or_default();
            let response = list_voices().await?;
            serde_json::to_value(response).context("序列化 list_voices 响应失败")
        }

        PLAY_OPERATION => {
            let req: PlayRequest = serde_json::from_value(payload).context("解析 play 请求失败")?;
            let result = play(req)?;
            serde_json::to_value(result).context("序列化 play 响应失败")
        }

        PLAY_STATUS_OPERATION => {
            let _payload: Empty = serde_json::from_value(payload).unwrap_or_default();
            let response = PlayStatusResponse {
                playing: play_running(),
            };
            serde_json::to_value(response).context("序列化 play_status 响应失败")
        }

        STOP_OPERATION => {
            let _payload: Empty = serde_json::from_value(payload).unwrap_or_default();
            stop_playback();
            serde_json::to_value(Empty {}).context("序列化 stop 响应失败")
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

/// 返回供应商音色列表（设置页选择用，调供应商 voices 接口）。
async fn list_voices() -> Result<ListVoicesResponse> {
    let models_config = tiangong_plugin_sidecar::model::load_models_config()?;
    let voices = tiangong_core::media::list_tts_voices(&models_config)
        .await
        .map_err(|e| anyhow::anyhow!("获取音色列表失败：{e}"))?;
    Ok(ListVoicesResponse {
        voices: voices
            .into_iter()
            .map(|v| VoiceInfo {
                id: v.id,
                name: v.name,
                gender: v.gender,
            })
            .collect(),
    })
}

/// 启动后台播放：立即返回，不等待播放完成。
///
/// stdio 分发是串行的，若本函数阻塞到播完，播放期间的 stop 请求会永远
/// 排在后面（停止按钮失效）、synthesize 也会被卡住。因此播放进程后台执行，
/// 完成状态经 `play_status` 轮询、经 `stop` 终止。已有播放未结束时先停旧的。
fn play(req: PlayRequest) -> Result<PlayResponse> {
    let path = std::path::Path::new(&req.file_path);
    if !path.exists() {
        anyhow::bail!("音频文件不存在：{}", req.file_path);
    }

    stop_playback();

    let mut command = play_command(&req.file_path);
    suppress_console_window(&mut command);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("播放失败：{e}"))
        .map(|child| {
            let mut guard = PLAYING
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(PlayState { child });
            PlayResponse { started: true }
        })
}

/// 按平台构造播放命令。
fn play_command(file_path: &str) -> std::process::Command {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("afplay");
        command.arg(file_path);
        command
    };

    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("powershell");
        command.args([
            "-c",
            &format!("(New-Object Media.SoundPlayer '{file_path}').PlaySync()"),
        ]);
        command
    };

    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = std::process::Command::new("aplay");
        command.arg(file_path);
        command
    };

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let mut command = std::process::Command::new("");

    let _ = &mut command;
    command
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
