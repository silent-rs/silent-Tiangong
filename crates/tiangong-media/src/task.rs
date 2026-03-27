use serde::{Deserialize, Serialize};

/// 媒体任务（用于管理异步长耗时任务）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaTask {
    /// 任务 ID
    pub id: String,
    /// 任务类型
    pub task_type: MediaTaskType,
    /// 任务状态
    pub status: MediaTaskStatus,
    /// 创建时间
    pub created_at: String,
    /// 完成时间
    pub completed_at: Option<String>,
    /// 结果 URL
    pub result_url: Option<String>,
    /// 错误信息
    pub error: Option<String>,
}

/// 媒体任务类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaTaskType {
    /// 图片生成
    ImageGeneration,
    /// 图片编辑
    ImageEdit,
    /// 视频生成
    VideoGeneration,
    /// 语音识别
    SpeechToText,
    /// 语音合成
    TextToSpeech,
}

/// 媒体任务状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MediaTaskStatus {
    /// 待处理
    Pending,
    /// 处理中
    Processing,
    /// 已完成
    Completed,
    /// 失败
    Failed,
}
