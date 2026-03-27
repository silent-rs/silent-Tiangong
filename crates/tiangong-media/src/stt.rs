use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 语音识别请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeRequest {
    /// 音频数据
    pub audio: Vec<u8>,
    /// MIME 类型（如 audio/wav、audio/mp3、audio/ogg）
    pub mime_type: String,
    /// 语言提示（如 "zh"、"en"）
    pub language: Option<String>,
    /// 指定模型
    pub model: Option<String>,
}

/// 语音识别响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeResponse {
    /// 识别出的文本
    pub text: String,
    /// 检测到的语言
    pub language: Option<String>,
    /// 音频时长（秒）
    pub duration: Option<f64>,
}

/// 语音识别能力 trait
#[async_trait]
pub trait SpeechRecognizer: Send + Sync {
    /// 识别器名称
    fn name(&self) -> &str;

    /// 将音频转录为文本
    async fn transcribe(&self, request: TranscribeRequest) -> Result<TranscribeResponse>;
}
