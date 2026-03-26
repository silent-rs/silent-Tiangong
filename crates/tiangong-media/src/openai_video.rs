use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::video::{VideoGenRequest, VideoGenStatus, VideoGenTask, VideoGenerator};

/// OpenAI/OpenRouter 兼容的视频生成后端
///
/// 使用异步任务模式：
/// 1. POST /v1/video/generations 提交生成任务
/// 2. GET /v1/video/generations/{id} 轮询任务状态
pub struct OpenAIVideoGenerator {
    api_key: String,
    api_base: String,
    model: String,
    client: Client,
}

impl OpenAIVideoGenerator {
    pub fn new(api_key: String, api_base: String, model: String) -> Self {
        Self {
            api_key,
            api_base,
            model,
            client: Client::new(),
        }
    }
}

#[derive(Debug, Serialize)]
struct VideoGenBody {
    model: String,
    prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<String>,
}

/// 提交任务的响应（OpenRouter 格式）
#[derive(Debug, Deserialize)]
struct VideoGenSubmitResponse {
    /// 生成 ID（OpenRouter: generation_id 或 id）
    #[serde(alias = "generation_id")]
    id: Option<String>,
    /// 直接返回的视频数据（某些 API 同步返回）
    data: Option<Vec<VideoGenData>>,
}

#[derive(Debug, Deserialize)]
struct VideoGenData {
    url: Option<String>,
}

/// 轮询任务状态的响应
#[derive(Debug, Deserialize)]
struct VideoGenStatusResponse {
    /// 状态：pending / processing / completed / failed
    status: Option<String>,
    /// 完成后的视频数据
    data: Option<Vec<VideoGenData>>,
    /// 错误信息
    error: Option<serde_json::Value>,
}

#[async_trait]
impl VideoGenerator for OpenAIVideoGenerator {
    fn name(&self) -> &str {
        "openai-video"
    }

    async fn generate(&self, request: VideoGenRequest) -> Result<VideoGenTask> {
        let url = format!("{}/v1/video/generations", self.api_base.trim_end_matches('/'));

        let body = VideoGenBody {
            model: request.model.unwrap_or_else(|| self.model.clone()),
            prompt: request.prompt,
            duration: request.duration,
            resolution: request.resolution,
        };

        debug!(url = %url, model = %body.model, "提交视频生成任务");

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("视频生成请求发送失败")?;

        let status_code = resp.status();
        let resp_text = resp.text().await.context("读取视频生成响应失败")?;

        debug!(status = %status_code, body = %resp_text, "视频生成提交响应");

        if !status_code.is_success() {
            return Err(anyhow!(
                "视频生成 API 返回错误 {}: {}",
                status_code,
                resp_text
            ));
        }

        let parsed: VideoGenSubmitResponse =
            serde_json::from_str(&resp_text).context("解析视频生成响应失败")?;

        // 某些 API 直接同步返回结果
        if let Some(data) = parsed.data
            && let Some(first) = data.first()
            && let Some(video_url) = &first.url
        {
            return Ok(VideoGenTask {
                task_id: parsed.id.unwrap_or_default(),
                status: VideoGenStatus::Completed {
                    video_url: video_url.clone(),
                    duration: None,
                },
            });
        }

        // 异步模式：返回 task_id 用于轮询
        let task_id = parsed
            .id
            .ok_or_else(|| anyhow!("视频生成响应缺少 id/generation_id"))?;

        info!(task_id = %task_id, "视频生成任务已提交，开始轮询");

        Ok(VideoGenTask {
            task_id,
            status: VideoGenStatus::Pending,
        })
    }

    async fn query_status(&self, task_id: &str) -> Result<VideoGenStatus> {
        let url = format!(
            "{}/v1/video/generations/{}",
            self.api_base.trim_end_matches('/'),
            task_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .context("视频任务状态查询失败")?;

        let status_code = resp.status();
        let resp_text = resp.text().await.context("读取状态响应失败")?;

        if !status_code.is_success() {
            return Err(anyhow!(
                "状态查询返回错误 {}: {}",
                status_code,
                resp_text
            ));
        }

        let parsed: VideoGenStatusResponse =
            serde_json::from_str(&resp_text).context("解析状态响应失败")?;

        let status_str = parsed.status.unwrap_or_default();
        match status_str.as_str() {
            "completed" | "complete" => {
                let video_url = parsed
                    .data
                    .and_then(|d| d.first().and_then(|v| v.url.clone()))
                    .unwrap_or_default();
                Ok(VideoGenStatus::Completed {
                    video_url,
                    duration: None,
                })
            }
            "failed" | "error" => {
                let error = parsed
                    .error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "未知错误".to_string());
                Ok(VideoGenStatus::Failed { error })
            }
            "processing" | "in_progress" => Ok(VideoGenStatus::Processing { progress: None }),
            _ => Ok(VideoGenStatus::Pending),
        }
    }
}
