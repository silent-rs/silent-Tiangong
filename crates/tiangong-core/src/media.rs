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
    use tiangong_media::image::ImageGenerator;

    let resolved = resolve_media_model(models_config, ModelCapability::ImageGeneration)?;
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

    Ok(MediaCallOutput { resolved, response })
}

pub async fn synthesize_speech(
    models_config: &ModelsConfig,
    text: String,
    voice: Option<String>,
    speed: Option<f64>,
    output_format: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::tts::SynthesizeResponse>, MediaServiceError> {
    use tiangong_media::tts::SpeechSynthesizer;

    let resolved = resolve_media_model(models_config, ModelCapability::Tts)?;
    let synthesizer = tiangong_media::openai_tts::OpenAITTS::new(
        resolved.api_key.clone(),
        resolved.base_url.clone(),
    );
    let request = tiangong_media::tts::SynthesizeRequest {
        text,
        voice: resolved_voice(&resolved, voice),
        speed,
        model: Some(resolved.model.clone()),
        output_format,
    };

    let response = tokio::time::timeout(Duration::from_secs(60), synthesizer.synthesize(request))
        .await
        .map_err(|_| MediaServiceError::Timeout("语音合成超时（60秒）".to_string()))?
        .map_err(|e| MediaServiceError::Backend(e.to_string()))?;

    Ok(MediaCallOutput { resolved, response })
}

pub async fn transcribe_audio(
    models_config: &ModelsConfig,
    audio: Vec<u8>,
    mime_type: String,
    language: Option<String>,
) -> Result<MediaCallOutput<tiangong_media::stt::TranscribeResponse>, MediaServiceError> {
    use tiangong_media::stt::SpeechRecognizer;

    let resolved = resolve_media_model(models_config, ModelCapability::Stt)?;
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

    Ok(MediaCallOutput { resolved, response })
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
