use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 视频生成请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenRequest {
    /// 生成提示词
    pub prompt: String,
    /// 时长（秒）
    pub duration: Option<u32>,
    /// 分辨率（如 "1080p"、"720p"）
    pub resolution: Option<String>,
    /// 指定模型
    pub model: Option<String>,
    /// 参考图片数据
    pub reference_image: Option<Vec<u8>>,
}

/// 视频生成任务（异步任务，提交后通过 task_id 轮询状态）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenTask {
    /// 任务 ID
    pub task_id: String,
    /// 任务状态
    pub status: VideoGenStatus,
}

/// 视频生成任务状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VideoGenStatus {
    /// 排队中
    Pending,
    /// 生成中，附带进度百分比
    Processing { progress: Option<f64> },
    /// 已完成
    Completed {
        /// 视频 URL
        video_url: String,
        /// 视频时长（秒）
        duration: Option<f64>,
    },
    /// 失败
    Failed {
        /// 错误信息
        error: String,
    },
}

/// 视频生成能力 trait
#[async_trait]
pub trait VideoGenerator: Send + Sync {
    /// 生成器名称
    fn name(&self) -> &str;

    /// 提交视频生成任务
    async fn generate(&self, request: VideoGenRequest) -> Result<VideoGenTask>;

    /// 查询视频生成任务状态
    async fn query_status(&self, task_id: &str) -> Result<VideoGenStatus>;
}
