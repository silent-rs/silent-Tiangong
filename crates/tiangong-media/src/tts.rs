use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 语音合成请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizeRequest {
    /// 待合成的文本
    pub text: String,
    /// 音色 ID
    pub voice: Option<String>,
    /// 语速（0.5~2.0）
    pub speed: Option<f64>,
    /// 指定模型
    pub model: Option<String>,
    /// 输出格式（mp3、wav、opus 等）
    pub output_format: Option<String>,
}

/// 语音合成响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizeResponse {
    /// 合成的音频数据
    pub audio: Vec<u8>,
    /// MIME 类型
    pub mime_type: String,
    /// 音频时长（秒）
    pub duration: Option<f64>,
}

/// 可用音色信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceInfo {
    /// 音色 ID
    pub id: String,
    /// 音色名称
    pub name: String,
    /// 语言
    pub language: Option<String>,
    /// 性别
    pub gender: Option<String>,
    /// 预览音频 URL
    pub preview_url: Option<String>,
}

/// 语音合成能力 trait
#[async_trait]
pub trait SpeechSynthesizer: Send + Sync {
    /// 合成器名称
    fn name(&self) -> &str;

    /// 将文本合成为语音
    async fn synthesize(&self, request: SynthesizeRequest) -> Result<SynthesizeResponse>;

    /// 列出可用的音色
    async fn list_voices(&self) -> Result<Vec<VoiceInfo>>;
}
