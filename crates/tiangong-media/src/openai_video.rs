use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::video::{VideoGenRequest, VideoGenStatus, VideoGenTask, VideoGenerator};

/// OpenAI-compatible 视频生成后端。
///
/// 不把 MCP 特化为唯一实现；MCP、OpenAI-compatible HTTP 服务或后续本地后端都应作为
/// `tiangong-media` 的后端适配器挂入同一层语义。
pub struct OpenAIVideoGenerator {
    api_key: String,
    api_base: String,
    model: String,
    endpoint_path: String,
    client: Client,
}

impl OpenAIVideoGenerator {
    pub fn new(
        api_key: String,
        api_base: String,
        model: String,
        endpoint_path: Option<String>,
    ) -> Self {
        Self {
            api_key,
            api_base,
            model,
            endpoint_path: normalize_endpoint_path(endpoint_path.as_deref()),
            client: Client::new(),
        }
    }

    fn endpoint_url(&self) -> String {
        format!(
            "{}{}",
            self.api_base.trim_end_matches('/'),
            self.endpoint_path
        )
    }

    fn task_url(&self, task_id: &str) -> String {
        format!("{}/{}", self.endpoint_url().trim_end_matches('/'), task_id)
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

#[derive(Debug, Deserialize)]
struct OpenAIErrorResponse {
    error: OpenAIError,
}

#[derive(Debug, Deserialize)]
struct OpenAIError {
    message: String,
}

#[async_trait]
impl VideoGenerator for OpenAIVideoGenerator {
    fn name(&self) -> &str {
        "openai-video"
    }

    async fn generate(&self, request: VideoGenRequest) -> Result<VideoGenTask> {
        let body = VideoGenBody {
            model: request.model.unwrap_or_else(|| self.model.clone()),
            prompt: request.prompt,
            duration: request.duration,
            resolution: request.resolution,
        };

        debug!("OpenAI-compatible 视频生成请求: {:?}", body);

        let resp = self
            .client
            .post(self.endpoint_url())
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .context("请求视频生成 API 失败")?;

        let status = resp.status();
        let resp_text = resp.text().await.context("读取响应体失败")?;

        if !status.is_success() {
            return Err(anyhow!(
                "视频生成失败 ({}): {}",
                status,
                parse_error_message(&resp_text)
            ));
        }

        let value: Value = serde_json::from_str(&resp_text).context("解析视频生成响应失败")?;
        parse_video_task(&value).ok_or_else(|| anyhow!("视频生成响应缺少任务或视频 URL"))
    }

    async fn query_status(&self, task_id: &str) -> Result<VideoGenStatus> {
        let resp = self
            .client
            .get(self.task_url(task_id))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .context("查询视频生成任务失败")?;

        let status = resp.status();
        let resp_text = resp.text().await.context("读取响应体失败")?;

        if !status.is_success() {
            return Err(anyhow!(
                "查询视频生成任务失败 ({}): {}",
                status,
                parse_error_message(&resp_text)
            ));
        }

        let value: Value = serde_json::from_str(&resp_text).context("解析视频任务状态失败")?;
        parse_video_status(&value).ok_or_else(|| anyhow!("视频任务响应缺少 status 字段"))
    }
}

fn normalize_endpoint_path(path: Option<&str>) -> String {
    let path = path
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or("/videos/generations");
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn parse_error_message(text: &str) -> String {
    serde_json::from_str::<OpenAIErrorResponse>(text)
        .map(|err| err.error.message)
        .unwrap_or_else(|_| text.to_string())
}

fn parse_video_task(value: &Value) -> Option<VideoGenTask> {
    let status = parse_video_status(value)?;
    let task_id = find_key_string(value, &["task_id", "id", "job_id"])
        .unwrap_or_else(|| "completed-video".into());
    Some(VideoGenTask { task_id, status })
}

fn parse_video_status(value: &Value) -> Option<VideoGenStatus> {
    if let Some(video_url) = find_url(value, &["video_url", "url", "output_url"]) {
        return Some(VideoGenStatus::Completed {
            video_url,
            duration: find_f64(value, &["duration"]),
        });
    }

    let status = find_key_string(value, &["status", "state"])?
        .to_ascii_lowercase()
        .replace('-', "_");
    match status.as_str() {
        "queued" | "pending" | "created" => Some(VideoGenStatus::Pending),
        "running" | "processing" | "in_progress" => Some(VideoGenStatus::Processing {
            progress: find_f64(value, &["progress", "percent"]),
        }),
        "completed" | "succeeded" | "success" | "done" => {
            find_url(value, &["video_url", "url", "output_url"]).map(|video_url| {
                VideoGenStatus::Completed {
                    video_url,
                    duration: find_f64(value, &["duration"]),
                }
            })
        }
        "failed" | "error" | "cancelled" | "canceled" => Some(VideoGenStatus::Failed {
            error: find_key_string(value, &["error", "message"])
                .unwrap_or_else(|| "视频生成失败".to_string()),
        }),
        _ => None,
    }
}

fn find_key_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(Value::String(text)) = map.get(*key)
                    && !text.trim().is_empty()
                {
                    return Some(text.clone());
                }
            }
            for child in map.values() {
                if let Some(text) = find_key_string(child, keys) {
                    return Some(text);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|item| find_key_string(item, keys)),
        _ => None,
    }
}

fn find_url(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::String(text) if text.starts_with("http://") || text.starts_with("https://") => {
            Some(text.clone())
        }
        Value::Object(map) => {
            for key in keys {
                if let Some(Value::String(text)) = map.get(*key)
                    && (text.starts_with("http://") || text.starts_with("https://"))
                {
                    return Some(text.clone());
                }
            }
            map.values().find_map(|child| find_url(child, keys))
        }
        Value::Array(items) => items.iter().find_map(|item| find_url(item, keys)),
        _ => None,
    }
}

fn find_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(item) = map.get(*key) {
                    if let Some(n) = item.as_f64() {
                        return Some(n);
                    }
                    if let Some(text) = item.as_str()
                        && let Ok(n) = text.parse::<f64>()
                    {
                        return Some(n);
                    }
                }
            }
            map.values().find_map(|child| find_f64(child, keys))
        }
        Value::Array(items) => items.iter().find_map(|item| find_f64(item, keys)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_completed_video_response() {
        let value = serde_json::json!({
            "id": "task-1",
            "status": "completed",
            "data": [{ "url": "https://example.invalid/video.mp4" }]
        });

        let task = parse_video_task(&value).expect("task");
        assert_eq!(task.task_id, "task-1");
        assert!(matches!(
            task.status,
            VideoGenStatus::Completed { ref video_url, .. }
                if video_url == "https://example.invalid/video.mp4"
        ));
    }
}
