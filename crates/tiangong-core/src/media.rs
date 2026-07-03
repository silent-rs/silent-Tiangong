use std::fmt;
use std::time::Duration;

use crate::models_config::{ModelCapability, ModelsConfig, ResolvedModel};

pub struct MediaCallOutput<T> {
    pub resolved: ResolvedModel,
    pub response: T,
}

#[derive(Debug)]
pub enum MediaServiceError {
    Config(String),
    Timeout(String),
    Backend(String),
}

impl MediaServiceError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }

    pub fn is_config(&self) -> bool {
        matches!(self, Self::Config(_))
    }
}

impl fmt::Display for MediaServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) | Self::Timeout(message) | Self::Backend(message) => {
                write!(f, "{message}")
            }
        }
    }
}

impl std::error::Error for MediaServiceError {}

fn resolve_media_model(
    models_config: &ModelsConfig,
    capability: ModelCapability,
) -> Result<ResolvedModel, MediaServiceError> {
    models_config
        .resolve_for_capability(capability)
        .ok_or_else(|| {
            MediaServiceError::Config(format!("{}能力未配置", capability.display_name()))
        })
}

fn resolved_voice(resolved: &ResolvedModel, voice: Option<String>) -> Option<String> {
    voice.or_else(|| {
        resolved
            .options
            .get("voice")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    })
}

pub async fn generate_image(
    models_config: &ModelsConfig,
    prompt: String,
    width: u32,
    height: u32,
    style: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::image::ImageGenResponse>, MediaServiceError> {
    let resolved = resolve_media_model(models_config, ModelCapability::ImageGeneration)?;
    generate_image_with(&resolved, prompt, width, height, style).await
}

/// 与 [`generate_image`] 相同，但直接接受已解析的端点，供插件复用（免去路由解析）。
pub async fn generate_image_with(
    resolved: &ResolvedModel,
    prompt: String,
    width: u32,
    height: u32,
    style: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::image::ImageGenResponse>, MediaServiceError> {
    use tiangong_media::image::ImageGenerator;

    let generator = tiangong_media::openai_image::OpenAIImageGenerator::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
        resolved.model.clone(),
    );
    let request = tiangong_media::image::ImageGenRequest {
        prompt,
        negative_prompt: None,
        width,
        height,
        model: Some(resolved.model.clone()),
        style,
        num_images: 1,
    };

    let response = tokio::time::timeout(Duration::from_secs(120), generator.generate(request))
        .await
        .map_err(|_| MediaServiceError::Timeout("图片生成超时（120秒）".to_string()))?
        .map_err(|e| MediaServiceError::Backend(e.to_string()))?;

    Ok(MediaCallOutput {
        resolved: resolved.clone(),
        response,
    })
}

pub async fn generate_video(
    models_config: &ModelsConfig,
    prompt: String,
    duration: Option<u32>,
    resolution: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::video::VideoGenTask>, MediaServiceError> {
    let resolved = resolve_media_model(models_config, ModelCapability::VideoGeneration)?;
    generate_video_with(&resolved, prompt, duration, resolution).await
}

/// 与 [`generate_video`] 相同，但直接接受已解析的端点，供插件复用（免去路由解析）。
pub async fn generate_video_with(
    resolved: &ResolvedModel,
    prompt: String,
    duration: Option<u32>,
    resolution: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::video::VideoGenTask>, MediaServiceError> {
    use tiangong_media::video::{VideoGenStatus, VideoGenerator};

    let endpoint_path = resolved
        .options
        .get("endpoint_path")
        .or_else(|| resolved.options.get("video_generation_path"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let poll_timeout_secs = resolved
        .options
        .get("poll_timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(180);
    let poll_interval_ms = resolved
        .options
        .get("poll_interval_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(2_000);
    let generator = tiangong_media::openai_video::OpenAIVideoGenerator::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
        resolved.model.clone(),
        endpoint_path,
    );
    let request = tiangong_media::video::VideoGenRequest {
        prompt,
        duration,
        resolution,
        model: Some(resolved.model.clone()),
        reference_image: None,
    };

    let mut task = tokio::time::timeout(Duration::from_secs(60), generator.generate(request))
        .await
        .map_err(|_| MediaServiceError::Timeout("提交视频生成任务超时（60秒）".to_string()))?
        .map_err(|e| MediaServiceError::Backend(e.to_string()))?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(poll_timeout_secs);
    while matches!(
        task.status,
        VideoGenStatus::Pending | VideoGenStatus::Processing { .. }
    ) && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(poll_interval_ms.max(500))).await;
        match generator.query_status(&task.task_id).await {
            Ok(status) => task.status = status,
            Err(err) => {
                tracing::warn!(task_id = %task.task_id, error = %err, "视频任务状态查询失败，保留已提交任务状态");
                break;
            }
        }
    }

    Ok(MediaCallOutput {
        resolved: resolved.clone(),
        response: task,
    })
}

pub async fn synthesize_speech(
    models_config: &ModelsConfig,
    text: String,
    voice: Option<String>,
    speed: Option<f64>,
    output_format: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::tts::SynthesizeResponse>, MediaServiceError> {
    let resolved = resolve_media_model(models_config, ModelCapability::Tts)?;
    synthesize_speech_with(&resolved, text, voice, speed, output_format).await
}

/// 与 [`synthesize_speech`] 相同，但直接接受已解析的端点，供插件复用（免去路由解析）。
pub async fn synthesize_speech_with(
    resolved: &ResolvedModel,
    text: String,
    voice: Option<String>,
    speed: Option<f64>,
    output_format: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::tts::SynthesizeResponse>, MediaServiceError> {
    use tiangong_media::tts::SpeechSynthesizer;

    let synthesizer = tiangong_media::openai_tts::OpenAITTS::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
    );
    let request = tiangong_media::tts::SynthesizeRequest {
        text,
        voice: resolved_voice(resolved, voice),
        speed,
        model: Some(resolved.model.clone()),
        output_format,
    };

    let response = tokio::time::timeout(Duration::from_secs(60), synthesizer.synthesize(request))
        .await
        .map_err(|_| MediaServiceError::Timeout("语音合成超时（60秒）".to_string()))?
        .map_err(|e| MediaServiceError::Backend(e.to_string()))?;

    Ok(MediaCallOutput {
        resolved: resolved.clone(),
        response,
    })
}

pub async fn transcribe_audio(
    models_config: &ModelsConfig,
    audio: Vec<u8>,
    mime_type: String,
    language: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::stt::TranscribeResponse>, MediaServiceError> {
    let resolved = resolve_media_model(models_config, ModelCapability::Stt)?;
    transcribe_audio_with(&resolved, audio, mime_type, language).await
}

/// 与 [`transcribe_audio`] 相同，但直接接受已解析的端点，供插件复用（免去路由解析）。
pub async fn transcribe_audio_with(
    resolved: &ResolvedModel,
    audio: Vec<u8>,
    mime_type: String,
    language: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::stt::TranscribeResponse>, MediaServiceError> {
    use tiangong_media::stt::SpeechRecognizer;

    let recognizer = tiangong_media::openai_stt::OpenAIWhisper::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
    );
    let request = tiangong_media::stt::TranscribeRequest {
        audio,
        mime_type,
        language,
        model: Some(resolved.model.clone()),
    };

    let response = tokio::time::timeout(Duration::from_secs(120), recognizer.transcribe(request))
        .await
        .map_err(|_| MediaServiceError::Timeout("语音识别超时（120秒）".to_string()))?
        .map_err(|e| MediaServiceError::Backend(e.to_string()))?;

    Ok(MediaCallOutput {
        resolved: resolved.clone(),
        response,
    })
}

pub async fn list_tts_voices(
    models_config: &ModelsConfig,
) -> Result<Vec<tiangong_media::tts::VoiceInfo>, MediaServiceError> {
    use tiangong_media::tts::SpeechSynthesizer;

    let resolved = resolve_media_model(models_config, ModelCapability::Tts)?;
    let synthesizer = tiangong_media::openai_tts::OpenAITTS::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
    );

    tokio::time::timeout(Duration::from_secs(10), synthesizer.list_voices())
        .await
        .map_err(|_| MediaServiceError::Timeout("获取音色列表超时".to_string()))?
        .map_err(|e| MediaServiceError::Backend(e.to_string()))
}
