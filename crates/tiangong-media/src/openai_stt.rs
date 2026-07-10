use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tracing::debug;

use crate::stt::{SpeechRecognizer, TranscribeRequest, TranscribeResponse};

/// OpenAI Whisper 语音识别后端
pub struct OpenAIWhisper {
    api_key: String,
    api_base: String,
    client: Client,
}

impl OpenAIWhisper {
    /// 创建 OpenAI Whisper 语音识别器
    ///
    /// - `api_key`: OpenAI API Key
    /// - `api_base`: API 基础地址，如 `https://api.openai.com`
    pub fn new(api_key: String, api_base: String) -> Self {
        Self {
            api_key,
            api_base,
            client: Client::new(),
        }
    }
}

/// 根据 MIME 类型推断文件扩展名
fn extension_from_mime(mime_type: &str) -> &str {
    match mime_type {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mp3" | "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/webm" => "webm",
        "audio/mp4" | "audio/m4a" => "m4a",
        _ => "wav",
    }
}

/// OpenAI 转录 API 响应
#[derive(Debug, Deserialize)]
struct WhisperResponse {
    text: String,
    language: Option<String>,
    duration: Option<f64>,
}

/// OpenAI 错误响应
#[derive(Debug, Deserialize)]
struct OpenAIErrorResponse {
    error: OpenAIError,
}

#[derive(Debug, Deserialize)]
struct OpenAIError {
    message: String,
}

#[async_trait]
impl SpeechRecognizer for OpenAIWhisper {
    fn name(&self) -> &str {
        "openai-whisper"
    }

    async fn transcribe(&self, request: TranscribeRequest) -> Result<TranscribeResponse> {
        let url = format!(
            "{}/audio/transcriptions",
            self.api_base.trim_end_matches('/')
        );
        let model = request.model.unwrap_or_else(|| "whisper-1".to_string());
        let ext = extension_from_mime(&request.mime_type);
        let filename = format!("audio.{}", ext);

        let file_part = reqwest::multipart::Part::bytes(request.audio)
            .file_name(filename)
            .mime_str(&request.mime_type)
            .context("构造音频文件部分失败")?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", file_part)
            .part("model", reqwest::multipart::Part::text(model))
            .part("response_format", reqwest::multipart::Part::text("json"));

        if let Some(lang) = request.language {
            form = form.part("language", reqwest::multipart::Part::text(lang));
        }

        debug!("OpenAI Whisper 转录请求: url={}", url);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .multipart(form)
            .send()
            .await
            .context("请求 OpenAI Whisper API 失败")?;

        let status = resp.status();
        let resp_text = resp.text().await.context("读取响应体失败")?;

        if !status.is_success() {
            let err_msg = serde_json::from_str::<OpenAIErrorResponse>(&resp_text)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| resp_text.clone());
            return Err(anyhow!("OpenAI Whisper 转录失败 ({}): {}", status, err_msg));
        }

        let whisper_resp: WhisperResponse =
            serde_json::from_str(&resp_text).with_context(|| {
                format!(
                    "解析 Whisper 响应失败，原始响应：{}",
                    resp_text.chars().take(500).collect::<String>()
                )
            })?;

        Ok(TranscribeResponse {
            text: whisper_resp.text,
            language: whisper_resp.language,
            duration: whisper_resp.duration,
        })
    }
}
