use anyhow::{Result, anyhow};
use async_trait::async_trait;

use crate::video::{VideoGenRequest, VideoGenStatus, VideoGenTask, VideoGenerator};

/// 视频生成占位后端
///
/// 当前无稳定的公开视频生成 API（Sora/Kling 等尚未开放），
/// 此实现作为占位，所有操作均返回错误。
pub struct StubVideoGenerator;

impl StubVideoGenerator {
    /// 创建占位视频生成器
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubVideoGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VideoGenerator for StubVideoGenerator {
    fn name(&self) -> &str {
        "stub-video"
    }

    async fn generate(&self, _request: VideoGenRequest) -> Result<VideoGenTask> {
        Err(anyhow!("视频生成后端未配置"))
    }

    async fn query_status(&self, _task_id: &str) -> Result<VideoGenStatus> {
        Err(anyhow!("视频生成后端未配置"))
    }
}
