use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::balance::Balance;
use crate::chat::Chat;
use crate::config::DeepSeekConfig;
use crate::error::DeepSeekError;
use crate::models::Models;

#[derive(Clone)]
pub struct DeepSeekClient {
    http_client: reqwest::Client,
    stream_http_client: reqwest::Client,
    config: DeepSeekConfig,
}

impl DeepSeekClient {
    pub fn from_config(config: DeepSeekConfig) -> Result<Self, DeepSeekError> {
        let http_client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|err| DeepSeekError::Transport(err.to_string()))?;
        let stream_http_client = reqwest::Client::builder()
            .build()
            .map_err(|err| DeepSeekError::Transport(err.to_string()))?;
        Ok(Self {
            http_client,
            stream_http_client,
            config,
        })
    }

    // ── 能力访问器 ──────────────────────────────────────────

    pub fn chat(&self) -> Chat<'_> {
        Chat::new(self)
    }

    pub fn models(&self) -> Models<'_> {
        Models::new(self)
    }

    pub fn balance(&self) -> Balance<'_> {
        Balance::new(self)
    }

    // ── 通用 HTTP 方法 ──────────────────────────────────────

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, DeepSeekError> {
        let url = self.build_url(path);
        let response = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send()
            .await
            .map_err(map_reqwest_error)?;
        parse_json_response(response).await
    }

    pub(crate) async fn post<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, DeepSeekError> {
        let url = self.build_url(path);
        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        parse_json_response(response).await
    }

    pub(crate) async fn post_stream_raw(
        &self,
        path: &str,
        body: &(impl Serialize + ?Sized),
    ) -> Result<reqwest::Response, DeepSeekError> {
        let url = self.build_url(path);
        let response = self
            .stream_http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(body)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        if !response.status().is_success() {
            return Err(parse_error_response(response).await);
        }

        Ok(response)
    }

    fn build_url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

fn map_reqwest_error(err: reqwest::Error) -> DeepSeekError {
    if err.is_timeout() {
        DeepSeekError::Transport(format!("timeout: {err}"))
    } else {
        DeepSeekError::Transport(err.to_string())
    }
}

async fn parse_error_response(response: reqwest::Response) -> DeepSeekError {
    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();
    let body_text = String::from_utf8_lossy(&body).to_string();
    classify_http_error(status, body_text)
}

async fn parse_json_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, DeepSeekError> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|err| DeepSeekError::Transport(err.to_string()))?;

    if !status.is_success() {
        let body_text = String::from_utf8_lossy(&body).to_string();
        return Err(classify_http_error(status, body_text));
    }

    serde_json::from_slice(&body).map_err(|err| {
        DeepSeekError::Serialization(format!(
            "{err}: {}",
            String::from_utf8_lossy(&body[..body.len().min(512)])
        ))
    })
}

fn classify_http_error(status: reqwest::StatusCode, body_text: String) -> DeepSeekError {
    if is_rate_limited_status_or_body(status, &body_text) {
        return DeepSeekError::RateLimited(body_text);
    }

    match status.as_u16() {
        400 => DeepSeekError::InvalidRequest(body_text),
        401 | 403 => DeepSeekError::Authentication(body_text),
        _ if status.is_server_error() => DeepSeekError::Transport(body_text),
        _ => DeepSeekError::Api(body_text),
    }
}

fn is_rate_limited_status_or_body(status: reqwest::StatusCode, body_text: &str) -> bool {
    let lower = body_text.to_ascii_lowercase();
    matches!(status.as_u16(), 429 | 529)
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("overloaded_error")
}
