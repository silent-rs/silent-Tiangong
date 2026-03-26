use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::video::{VideoGenRequest, VideoGenStatus, VideoGenTask, VideoGenerator};

/// OpenAI/OpenRouter 兼容的视频生成后端
///
/// 通过 chat completions 接口调用视频生成模型。
/// OpenRouter 等平台的视频模型统一使用 /chat/completions 端点，
/// 返回异步 generation_id 需轮询获取结果。
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

    fn completions_url(&self) -> String {
        let base = self.api_base.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/chat/completions", base)
        } else {
            format!("{}/v1/chat/completions", base)
        }
    }

    fn generation_url(&self, gen_id: &str) -> String {
        // OpenRouter: GET /api/v1/generation?id={gen_id}
        let base = self.api_base.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/generation?id={}", base, gen_id)
        } else {
            format!("{}/v1/generation?id={}", base, gen_id)
        }
    }
}

/// Chat completions 请求体
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// Chat completions 响应（视频模型返回）
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    /// 生成 ID（用于轮询）
    id: Option<String>,
    /// choices（可能包含视频 URL）
    choices: Option<Vec<ChatChoice>>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: Option<ChatChoiceMessage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
}

/// OpenRouter generation 查询响应
#[derive(Debug, Deserialize)]
struct GenerationQueryResponse {
    data: Option<GenerationData>,
}

#[derive(Debug, Deserialize)]
struct GenerationData {
    /// 完成原因
    finish_reason: Option<String>,
    /// 原始响应（包含视频 URL）
    native_response: Option<serde_json::Value>,
}

#[async_trait]
impl VideoGenerator for OpenAIVideoGenerator {
    fn name(&self) -> &str {
        "openai-video"
    }

    async fn generate(&self, request: VideoGenRequest) -> Result<VideoGenTask> {
        let url = self.completions_url();

        let body = ChatCompletionRequest {
            model: request.model.unwrap_or_else(|| self.model.clone()),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: request.prompt,
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
            // 截取错误信息前 500 字符，避免 HTML 页面太长
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

        let parsed: ChatCompletionResponse =
            serde_json::from_str(&resp_text).context("解析视频生成响应失败")?;

        // 检查是否直接返回了结果（同步模式）
        if let Some(choices) = &parsed.choices
            && let Some(choice) = choices.first()
            && let Some(msg) = &choice.message
            && let Some(content) = &msg.content
        {
            let trimmed = content.trim();
            if trimmed.starts_with("http")
                && (trimmed.contains(".mp4") || trimmed.contains("video"))
            {
                return Ok(VideoGenTask {
                    task_id: parsed.id.unwrap_or_default(),
                    status: VideoGenStatus::Completed {
                        video_url: trimmed.to_string(),
                        duration: None,
                    },
                });
            }
        }

        // 异步模式：返回 generation_id 用于轮询
        let gen_id = parsed
            .id
            .ok_or_else(|| anyhow!("视频生成响应缺少 id"))?;

        info!(generation_id = %gen_id, "视频生成任务已提交，开始轮询");

        Ok(VideoGenTask {
            task_id: gen_id,
            status: VideoGenStatus::Pending,
        })
    }

    async fn query_status(&self, task_id: &str) -> Result<VideoGenStatus> {
        let url = self.generation_url(task_id);

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .context("视频任务状态查询失败")?;

        let status_code = resp.status();
        let resp_text = resp.text().await.context("读取状态响应失败")?;

        debug!(status = %status_code, body_len = resp_text.len(), "视频状态查询响应");

        if !status_code.is_success() {
            return Err(anyhow!(
                "状态查询返回错误 {}: {}",
                status_code,
                if resp_text.len() > 300 { &resp_text[..300] } else { &resp_text }
            ));
        }

        let parsed: GenerationQueryResponse =
            serde_json::from_str(&resp_text).context("解析状态响应失败")?;

        let Some(data) = parsed.data else {
            return Ok(VideoGenStatus::Pending);
        };

        match data.finish_reason.as_deref() {
            Some("stop") | Some("complete") | Some("completed") => {
                // 从 native_response 中提取视频 URL
                let video_url = extract_video_url(&data.native_response)
                    .unwrap_or_default();
                if video_url.is_empty() {
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
            Some("error") => {
                let error = data
                    .native_response
                    .as_ref()
                    .and_then(|v| v.get("error"))
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "未知错误".to_string());
                Ok(VideoGenStatus::Failed { error })
            }
            _ => Ok(VideoGenStatus::Processing { progress: None }),
        }
    }
}

/// 从 native_response 中递归查找视频 URL
fn extract_video_url(value: &Option<serde_json::Value>) -> Option<String> {
    let val = value.as_ref()?;

    // 直接是字符串且像 URL
    if let Some(s) = val.as_str()
        && s.starts_with("http")
    {
        return Some(s.to_string());
    }

    // 在 JSON 中查找常见的视频 URL 字段
    for key in ["video_url", "url", "video", "output", "result", "download_url"] {
        if let Some(v) = val.get(key)
            && let Some(s) = v.as_str()
            && s.starts_with("http")
        {
            return Some(s.to_string());
        }
    }

    // 在数组中查找
    if let Some(arr) = val.as_array() {
        for item in arr {
            let result = extract_video_url(&Some(item.clone()));
            if result.is_some() {
                return result;
            }
        }
    }

    // 在 data 子对象中查找
    if let Some(data) = val.get("data") {
        return extract_video_url(&Some(data.clone()));
    }

    None
}
