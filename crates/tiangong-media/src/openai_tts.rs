use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::tts::{SpeechSynthesizer, SynthesizeRequest, SynthesizeResponse, VoiceInfo};

/// OpenAI TTS 语音合成后端
pub struct OpenAITTS {
    api_key: String,
    api_base: String,
    client: Client,
}

impl OpenAITTS {
    /// 创建 OpenAI TTS 语音合成器
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

/// OpenAI TTS 请求体
#[derive(Debug, Serialize)]
struct TTSRequestBody {
    model: String,
    input: String,
    voice: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<String>,
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

/// 获取输出格式对应的 MIME 类型
fn mime_for_format(format: &str) -> &str {
    match format {
        "mp3" => "audio/mpeg",
        "opus" => "audio/opus",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "pcm" => "audio/pcm",
        _ => "audio/mpeg",
    }
}

/// OpenAI 预定义音色列表
fn openai_voices() -> Vec<VoiceInfo> {
    let voices = [
        ("alloy", "Alloy", "neutral"),
        ("echo", "Echo", "male"),
        ("fable", "Fable", "male"),
        ("onyx", "Onyx", "male"),
        ("nova", "Nova", "female"),
        ("shimmer", "Shimmer", "female"),
    ];

    voices
        .into_iter()
        .map(|(id, name, gender)| VoiceInfo {
            id: id.to_string(),
            name: name.to_string(),
            language: None,
            gender: Some(gender.to_string()),
            preview_url: None,
        })
        .collect()
}

#[async_trait]
impl SpeechSynthesizer for OpenAITTS {
    fn name(&self) -> &str {
        "openai-tts"
    }

    async fn synthesize(&self, request: SynthesizeRequest) -> Result<SynthesizeResponse> {
        let url = format!("{}/audio/speech", self.api_base.trim_end_matches('/'));
        let voice = request.voice.unwrap_or_else(|| "Chinese Female".to_string());
        let model = request.model.unwrap_or_else(|| "tts-1".to_string());
        let output_format = request
            .output_format
            .clone()
            .unwrap_or_else(|| "mp3".to_string());

        let body = TTSRequestBody {
            model,
            input: request.text,
            voice,
            speed: request.speed,
            response_format: Some(output_format.clone()),
        };

        debug!("OpenAI TTS 合成请求: {:?}", body);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .context("请求 OpenAI TTS API 失败")?;

        let status = resp.status();

        if !status.is_success() {
            let resp_text = resp.text().await.context("读取响应体失败")?;
            let err_msg = serde_json::from_str::<OpenAIErrorResponse>(&resp_text)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| resp_text.clone());
            return Err(anyhow!("OpenAI TTS 合成失败 ({}): {}", status, err_msg));
        }

        let audio = resp.bytes().await.context("读取音频数据失败")?.to_vec();

        // 防止 API 返回 200 但内容是错误 JSON（某些非标准 TTS 提供商的行为）
        if audio.len() < 256 {
            if let Ok(text) = std::str::from_utf8(&audio) {
                if text.trim_start().starts_with('{') {
                    return Err(anyhow!("TTS API 返回错误：{}", text));
                }
            }
        }

        let mime_type = mime_for_format(&output_format).to_string();

        Ok(SynthesizeResponse {
            audio,
            mime_type,
            duration: None,
        })
    }

    async fn list_voices(&self) -> Result<Vec<VoiceInfo>> {
        // 尝试从 API 获取可用音色列表（VoxBox 等后端支持 /v1/voices）
        let url = format!("{}/voices", self.api_base.trim_end_matches('/'));
        if let Ok(resp) = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(voices) = body.get("voices").and_then(|v| v.as_array()) {
                        let list: Vec<VoiceInfo> = voices
                            .iter()
                            .filter_map(|v| {
                                let name = v.as_str().unwrap_or_default().to_string();
                                if name.is_empty() {
                                    return None;
                                }
                                Some(VoiceInfo {
                                    id: name.clone(),
                                    name,
                                    language: None,
                                    gender: None,
                                    preview_url: None,
                                })
                            })
                            .collect();
                        if !list.is_empty() {
                            return Ok(list);
                        }
                    }
                }
            }
        }
        // API 不支持 /v1/voices 时返回空列表，由前端显示手动输入框
        Ok(vec![])
    }
}

