use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::video::{VideoGenRequest, VideoGenStatus, VideoGenTask, VideoGenerator};

/// 通用视频生成后端（兼容火山方舟/OpenAI 等）
///
/// 自动探测 API 风格：
/// - 火山方舟：POST /contents/generations → 异步任务 → GET /contents/generations/{id}
/// - OpenAI 兼容：POST /chat/completions → messages 格式
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

    fn base_url(&self) -> &str {
        self.api_base.trim_end_matches('/')
    }
}

// ---------------------------------------------------------------------------
// 火山方舟 / Volcengine Ark 格式
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ArkVideoGenBody {
    model: String,
    content: Vec<ArkContent>,
}

#[derive(Debug, Serialize)]
struct ArkContent {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArkSubmitResponse {
    id: Option<String>,
    #[serde(default)]
    error: Option<ArkError>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ArkError {
    message: Option<String>,
    code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArkStatusResponse {
    #[allow(dead_code)]
    id: Option<String>,
    status: Option<String>,
    content: Option<ArkResponseContent>,
    error: Option<ArkError>,
}

#[derive(Debug, Deserialize)]
struct ArkResponseContent {
    video_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    duration: Option<f64>,
}

#[async_trait]
impl VideoGenerator for OpenAIVideoGenerator {
    fn name(&self) -> &str {
        "openai-video"
    }

    async fn generate(&self, request: VideoGenRequest) -> Result<VideoGenTask> {
        let base = self.base_url();

        // 构建 URL：在 base_url 后拼接 /contents/generations
        let url = format!("{}/contents/generations", base);

        let body = ArkVideoGenBody {
            model: request.model.unwrap_or_else(|| self.model.clone()),
            content: vec![ArkContent {
                content_type: "text".to_string(),
                text: Some(request.prompt),
            }],
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

        debug!(status = %status_code, body_len = resp_text.len(), "视频生成提交响应");

        if !status_code.is_success() {
            let err_preview = if resp_text.len() > 500 {
                format!("{}...", &resp_text[..500])
            } else {
                resp_text
            };
            return Err(anyhow!(
                "视频生成 API 返回错误 {}: {}",
                status_code,
                err_preview
            ));
        }

        let parsed: ArkSubmitResponse =
            serde_json::from_str(&resp_text).context("解析视频生成响应失败")?;

        if let Some(error) = &parsed.error {
            return Err(anyhow!(
                "视频生成返回错误：{} ({})",
                error.message.as_deref().unwrap_or("unknown"),
                error.code.as_deref().unwrap_or("")
            ));
        }

        let task_id = parsed
            .id
            .ok_or_else(|| anyhow!("视频生成响应缺少 id 字段"))?;

        info!(task_id = %task_id, "视频生成任务已提交，开始轮询");

        Ok(VideoGenTask {
            task_id,
            status: VideoGenStatus::Pending,
        })
    }

    async fn query_status(&self, task_id: &str) -> Result<VideoGenStatus> {
        let base = self.base_url();
        let url = format!("{}/contents/generations/{}", base, task_id);

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .context("视频任务状态查询失败")?;

        let status_code = resp.status();
        let resp_text = resp.text().await.context("读取状态响应失败")?;

        debug!(status = %status_code, "视频状态查询响应");

        if !status_code.is_success() {
            return Err(anyhow!("状态查询返回错误 {}", status_code));
        }

        let parsed: ArkStatusResponse =
            serde_json::from_str(&resp_text).context("解析状态响应失败")?;

        if let Some(error) = &parsed.error {
            return Ok(VideoGenStatus::Failed {
                error: error
                    .message
                    .clone()
                    .unwrap_or_else(|| "未知错误".to_string()),
            });
        }

        match parsed.status.as_deref() {
            Some("succeeded") | Some("completed") | Some("complete") => {
                let video_url = parsed
                    .content
                    .and_then(|c| c.video_url.or(c.url))
                    .unwrap_or_default();
                if video_url.is_empty() {
                    warn!("视频生成完成但未找到 URL，原始响应：{}", resp_text);
                    Ok(VideoGenStatus::Failed {
                        error: "生成完成但未找到视频 URL".to_string(),
                    })
                } else {
                    Ok(VideoGenStatus::Completed {
                        video_url,
                        duration: None,
                    })
                }
            }
            Some("failed") | Some("error") => {
                let error = parsed
                    .content
                    .and_then(|c| c.video_url) // 某些 API 在 content 中放错误信息
                    .unwrap_or_else(|| "生成失败".to_string());
                Ok(VideoGenStatus::Failed { error })
            }
            Some("running") | Some("processing") | Some("in_progress") => {
                Ok(VideoGenStatus::Processing { progress: None })
            }
            _ => Ok(VideoGenStatus::Pending),
        }
    }
}
