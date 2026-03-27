use anyhow::{Result, anyhow};

use crate::image::{ImageEditRequest, ImageGenRequest, ImageGenResponse, ImageGenerator};
use crate::stt::{SpeechRecognizer, TranscribeRequest, TranscribeResponse};
use crate::tts::{SpeechSynthesizer, SynthesizeRequest, SynthesizeResponse, VoiceInfo};
use crate::video::{VideoGenRequest, VideoGenStatus, VideoGenTask, VideoGenerator};

/// 统一的媒体能力管理器，聚合所有多媒体后端
pub struct MediaAgent {
    image_generator: Option<Box<dyn ImageGenerator>>,
    video_generator: Option<Box<dyn VideoGenerator>>,
    speech_recognizer: Option<Box<dyn SpeechRecognizer>>,
    speech_synthesizer: Option<Box<dyn SpeechSynthesizer>>,
}

impl MediaAgent {
    /// 创建空的 MediaAgent（所有后端均为 None）
    pub fn new() -> Self {
        Self {
            image_generator: None,
            video_generator: None,
            speech_recognizer: None,
            speech_synthesizer: None,
        }
    }

    /// 设置图片生成后端
    pub fn with_image_generator(mut self, backend: Box<dyn ImageGenerator>) -> Self {
        self.image_generator = Some(backend);
        self
    }

    /// 设置视频生成后端
    pub fn with_video_generator(mut self, backend: Box<dyn VideoGenerator>) -> Self {
        self.video_generator = Some(backend);
        self
    }

    /// 设置语音识别后端
    pub fn with_speech_recognizer(mut self, rec: Box<dyn SpeechRecognizer>) -> Self {
        self.speech_recognizer = Some(rec);
        self
    }

    /// 设置语音合成后端
    pub fn with_speech_synthesizer(mut self, syn: Box<dyn SpeechSynthesizer>) -> Self {
        self.speech_synthesizer = Some(syn);
        self
    }

    /// 生成图片
    pub async fn generate_image(&self, request: ImageGenRequest) -> Result<ImageGenResponse> {
        let backend = self
            .image_generator
            .as_ref()
            .ok_or_else(|| anyhow!("图片生成后端未配置"))?;
        backend.generate(request).await
    }

    /// 编辑图片
    pub async fn edit_image(&self, request: ImageEditRequest) -> Result<ImageGenResponse> {
        let backend = self
            .image_generator
            .as_ref()
            .ok_or_else(|| anyhow!("图片生成后端未配置"))?;
        backend.edit(request).await
    }

    /// 语音识别（音频转文本）
    pub async fn transcribe(&self, request: TranscribeRequest) -> Result<TranscribeResponse> {
        let rec = self
            .speech_recognizer
            .as_ref()
            .ok_or_else(|| anyhow!("语音识别后端未配置"))?;
        rec.transcribe(request).await
    }

    /// 语音合成（文本转语音）
    pub async fn synthesize(&self, request: SynthesizeRequest) -> Result<SynthesizeResponse> {
        let syn = self
            .speech_synthesizer
            .as_ref()
            .ok_or_else(|| anyhow!("语音合成后端未配置"))?;
        syn.synthesize(request).await
    }

    /// 列出可用音色
    pub async fn list_voices(&self) -> Result<Vec<VoiceInfo>> {
        let syn = self
            .speech_synthesizer
            .as_ref()
            .ok_or_else(|| anyhow!("语音合成后端未配置"))?;
        syn.list_voices().await
    }

    /// 提交视频生成任务
    pub async fn generate_video(&self, request: VideoGenRequest) -> Result<VideoGenTask> {
        let backend = self
            .video_generator
            .as_ref()
            .ok_or_else(|| anyhow!("视频生成后端未配置"))?;
        backend.generate(request).await
    }

    /// 查询视频生成任务状态
    pub async fn query_video_status(&self, task_id: &str) -> Result<VideoGenStatus> {
        let backend = self
            .video_generator
            .as_ref()
            .ok_or_else(|| anyhow!("视频生成后端未配置"))?;
        backend.query_status(task_id).await
    }

    /// 检查图片生成能力是否可用
    pub fn has_image_generator(&self) -> bool {
        self.image_generator.is_some()
    }

    /// 检查视频生成能力是否可用
    pub fn has_video_generator(&self) -> bool {
        self.video_generator.is_some()
    }

    /// 检查语音识别能力是否可用
    pub fn has_speech_recognizer(&self) -> bool {
        self.speech_recognizer.is_some()
    }

    /// 检查语音合成能力是否可用
    pub fn has_speech_synthesizer(&self) -> bool {
        self.speech_synthesizer.is_some()
    }
}

impl Default for MediaAgent {
    fn default() -> Self {
        Self::new()
    }
}
