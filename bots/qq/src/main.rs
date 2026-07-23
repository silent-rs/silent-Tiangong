//! 天工 QQ bot 制品。
//!
//! 独立编译的二进制，由天工主程序在运行时下载并启动。
//! 通过 QQ 开放平台 WebSocket 网关接收 QQ 消息（C2C 私聊、群组 @提及），
//! 转发到天工本地 embedded server 的 `POST /api/v1/messages`，
//! 再把天工返回的文本/本地文件回复发送回 QQ。
//!
//! 凭证由天工主程序通过环境变量注入，或由 bot 扫码配置自行保存：
//! - `TIANGONG_BOT_QQ_APP_ID` / `TIANGONG_BOT_QQ_APP_SECRET`
//! - `TIANGONG_URL`（默认 `http://127.0.0.1:8080`）
//! - `TIANGONG_TOKEN`（可选，embedded server 认证 token）

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::signal;
use tokio::sync::{Mutex, Notify};
use tracing_subscriber::EnvFilter;

mod gateway;
mod provision;
mod schema;
mod token;

use gateway::{DispatchEvent, GatewayRunner, INTENT_GROUP_AND_C2C_EVENT};
use token::AccessTokenCache;

const RECENT_MESSAGE_LIMIT: usize = 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const OPENAPI_BASE: &str = "https://api.sgroup.qq.com";

struct BotState {
    http: Client,
    token: AccessTokenCache,
    /// 天工 server 地址，如 http://127.0.0.1:8080
    tiangong_url: String,
    /// 天工 server 认证 token（可选）
    tiangong_token: Option<String>,
    recent_messages: Mutex<RecentMessages>,
}

#[derive(Default)]
struct RecentMessages {
    keys: VecDeque<String>,
}

impl RecentMessages {
    fn claim(&mut self, channel_id: &str, message_id: &str) -> bool {
        if channel_id.is_empty() || message_id.is_empty() {
            return false;
        }
        let key = format!("{channel_id}:{message_id}");
        if self.keys.iter().any(|existing| existing == &key) {
            return false;
        }
        self.keys.push_back(key);
        if self.keys.len() > RECENT_MESSAGE_LIMIT {
            self.keys.pop_front();
        }
        true
    }

    fn release(&mut self, channel_id: &str, message_id: &str) {
        let key = format!("{channel_id}:{message_id}");
        self.keys.retain(|existing| existing != &key);
    }
}

// ── 天工 Server API 类型 ──────────────────────────────────────

#[derive(Debug, Serialize)]
struct ConnectorRequest {
    connector: String,
    channel_id: String,
    sender_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ApiMessageContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    media: Vec<MediaAsset>,
}

#[derive(Debug, Deserialize)]
struct ConnectorResponse {
    #[allow(dead_code)]
    session_id: String,
    message: String,
    content: ApiMessageContent,
    #[serde(default)]
    attachments: Vec<ApiMessageContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiMessageContent {
    Text {
        text: String,
    },
    Image {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    File {
        url: String,
        name: String,
    },
    Audio {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<u32>,
    },
    Video {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
}

impl ApiMessageContent {
    fn text(&self) -> String {
        match self {
            Self::Text { text } => text.clone(),
            Self::Image { caption, .. } | Self::Video { caption, .. } => {
                caption.clone().unwrap_or_default()
            }
            Self::File { .. } | Self::Audio { .. } => String::new(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Text { text } => text.trim().is_empty(),
            Self::Image { url, .. }
            | Self::File { url, .. }
            | Self::Audio { url, .. }
            | Self::Video { url, .. } => url.trim().is_empty(),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Image { .. } => "image",
            Self::File { .. } => "file",
            Self::Audio { .. } => "audio",
            Self::Video { .. } => "video",
        }
    }

    fn local_file_url(&self) -> Option<&str> {
        match self {
            Self::Image { url, .. }
            | Self::File { url, .. }
            | Self::Audio { url, .. }
            | Self::Video { url, .. } => Some(url),
            Self::Text { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct MediaAsset {
    kind: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability: Option<String>,
}

impl MediaAsset {
    fn image(url: String, mime_type: String) -> Self {
        Self {
            kind: "image".to_string(),
            url,
            mime_type: Some(mime_type),
            title: None,
            capability: Some("multimodal".to_string()),
        }
    }
}

struct ParsedMessage {
    content: ApiMessageContent,
    media: Vec<MediaAsset>,
}

struct BotReply {
    text: String,
    files: Vec<PathBuf>,
}

// ── QQ 事件载荷 ──────────────────────────────────────────────
//
// QQ 官方事件 payload 的字段不全部被消费，部分字段保留用于协议完整性
// 与未来扩展，因此整个载荷族标记 `#[allow(dead_code)]`。

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GroupAtMessage {
    #[serde(default)]
    id: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    group_openid: String,
    #[serde(default)]
    message_type: String,
    author: MessageAuthor,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct C2cMessage {
    #[serde(default)]
    id: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    user_openid: String,
    #[serde(default)]
    author: MessageAuthor,
    #[serde(default)]
    attachments: Vec<QqAttachment>,
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct MessageAuthor {
    #[serde(default)]
    member_openid: String,
    #[serde(default)]
    user_openid: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct QqAttachment {
    #[serde(default)]
    content_type: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    filename: String,
}

// ── 入口 ──────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // 管理命令必须在日志和常驻运行初始化前处理，stdout 只输出协议 JSON。
    match std::env::args().nth(1).as_deref() {
        Some("--describe") => {
            println!("{}", serde_json::to_string(&schema::describe_output())?);
            return Ok(());
        }
        Some("--provision-begin") => {
            println!("{}", serde_json::to_string(&provision::begin().await?)?);
            return Ok(());
        }
        Some("--provision-poll") => {
            println!(
                "{}",
                serde_json::to_string(&provision::poll_from_stdin().await?)?
            );
            return Ok(());
        }
        _ => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(std::io::stderr)
        .init();

    let credentials = provision::load_credentials()?;
    let tiangong_url =
        std::env::var("TIANGONG_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let tiangong_token = std::env::var("TIANGONG_TOKEN")
        .ok()
        .filter(|token| !token.is_empty());

    tracing::info!("QQ 机器人启动中...");
    tracing::info!("天工服务地址: {tiangong_url}");

    let http = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("构建 HTTP 客户端失败")?;
    let token = AccessTokenCache::new(http.clone(), credentials.app_id, credentials.app_secret);

    let state = Arc::new(BotState {
        http: http.clone(),
        token,
        tiangong_url,
        tiangong_token,
        recent_messages: Mutex::new(RecentMessages::default()),
    });

    let shutdown = Arc::new(Notify::new());
    let runner = GatewayRunner::new(
        http,
        state.token.clone(),
        INTENT_GROUP_AND_C2C_EVENT,
        {
            let state = state.clone();
            Arc::new(move |event| {
                let state = state.clone();
                Box::pin(async move { handle_dispatch(&state, event).await })
            })
        },
        shutdown.clone(),
    );

    let gateway_handle = tokio::spawn(async move {
        runner.run().await;
    });

    #[cfg(unix)]
    {
        let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())
            .context("注册 SIGTERM 处理失败")?;
        let mut int = signal::unix::signal(signal::unix::SignalKind::interrupt())
            .context("注册 SIGINT 处理失败")?;
        tokio::select! {
            _ = term.recv() => tracing::info!("收到 SIGTERM，正在退出..."),
            _ = int.recv() => tracing::info!("收到 SIGINT，正在退出..."),
            _ = gateway_handle => tracing::info!("Gateway 主循环已结束"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = signal::ctrl_c() => tracing::info!("收到 Ctrl+C，正在退出..."),
            _ = gateway_handle => tracing::info!("Gateway 主循环已结束"),
        }
    }

    shutdown.notify_waiters();
    Ok(())
}

// ── 事件分派 ──────────────────────────────────────────────────

async fn handle_dispatch(state: &Arc<BotState>, event: DispatchEvent) {
    match event.event_type.as_str() {
        "GROUP_AT_MESSAGE_CREATE" => {
            if let Err(error) = handle_group_message(state, event.data, event.seq).await {
                tracing::error!("处理 QQ 群消息失败: {error}");
            }
        }
        "C2C_MESSAGE_CREATE" => {
            if let Err(error) = handle_c2c_message(state, event.data, event.seq).await {
                tracing::error!("处理 QQ 私聊消息失败: {error}");
            }
        }
        other => {
            tracing::debug!("忽略 QQ 事件类型: {other}");
        }
    }
}

async fn handle_group_message(state: &Arc<BotState>, data: Value, seq: Option<u64>) -> Result<()> {
    let message: GroupAtMessage = serde_json::from_value(data).context("解析 QQ 群消息失败")?;
    let message_id = message.id.trim();
    if message_id.is_empty() {
        tracing::warn!("QQ 群消息缺少 id，跳过");
        return Ok(());
    }
    let channel_id = format!("group:{}", message.group_openid);
    let sender_id = if !message.author.member_openid.is_empty() {
        message.author.member_openid.as_str()
    } else {
        message.author.user_openid.as_str()
    };
    if !state
        .recent_messages
        .lock()
        .await
        .claim(&channel_id, message_id)
    {
        tracing::debug!("忽略重复 QQ 群消息 id={message_id}");
        return Ok(());
    }

    let text = strip_at_mention(&message.content);
    if text.trim().is_empty() {
        tracing::debug!("QQ 群消息内容为空，跳过");
        return Ok(());
    }
    let parsed = ParsedMessage {
        content: ApiMessageContent::Text { text },
        media: Vec::new(),
    };

    let _ = seq;
    tracing::info!(
        "转发 QQ 群消息 group={} sender={sender_id} len={}",
        message.group_openid,
        parsed.content.text().len()
    );
    let group_openid = message.group_openid.clone();
    let reply = match forward_to_tiangong(state, &channel_id, sender_id, message_id, parsed).await {
        Ok(reply) => reply,
        Err(error) => {
            // 转发失败时释放去重占用，允许下次推送重试
            state
                .recent_messages
                .lock()
                .await
                .release(&channel_id, message_id);
            return Err(error);
        }
    };
    send_reply(state, ReplyTarget::Group(group_openid), message_id, reply).await
}

async fn handle_c2c_message(state: &Arc<BotState>, data: Value, seq: Option<u64>) -> Result<()> {
    let message: C2cMessage = serde_json::from_value(data).context("解析 QQ 私聊消息失败")?;
    let message_id = message.id.trim();
    if message_id.is_empty() {
        tracing::warn!("QQ 私聊消息缺少 id，跳过");
        return Ok(());
    }
    let channel_id = format!("c2c:{}", message.user_openid);
    let sender_id = message.author.user_openid.as_str();
    if !state
        .recent_messages
        .lock()
        .await
        .claim(&channel_id, message_id)
    {
        tracing::debug!("忽略重复 QQ 私聊消息 id={message_id}");
        return Ok(());
    }

    let text = message.content.trim().to_string();
    let mut downloaded_images = Vec::new();
    for attachment in &message.attachments {
        if attachment.content_type.starts_with("image/") {
            match download_attachment(state, attachment).await {
                Ok(image) => downloaded_images.push(image),
                Err(error) => tracing::warn!("下载 QQ 图片失败: {error}"),
            }
        }
    }

    let parsed = if !text.is_empty() {
        ParsedMessage {
            content: ApiMessageContent::Text { text },
            media: downloaded_images
                .into_iter()
                .map(|image| MediaAsset::image(image.url, image.mime_type))
                .collect(),
        }
    } else {
        let mut images = downloaded_images.into_iter();
        let Some(first) = images.next() else {
            tracing::debug!("QQ 私聊消息没有可处理的文本或图片，跳过");
            return Ok(());
        };
        ParsedMessage {
            content: ApiMessageContent::Image {
                url: first.url,
                caption: None,
            },
            media: images
                .map(|image| MediaAsset::image(image.url, image.mime_type))
                .collect(),
        }
    };

    if parsed.content.is_empty() {
        return Ok(());
    }
    let _ = seq;
    tracing::info!(
        "转发 QQ 私聊消息 user={} content_type={} images={}",
        message.user_openid,
        parsed.content.kind(),
        parsed.media.len()
    );
    let user_openid = message.user_openid.clone();
    let reply = match forward_to_tiangong(state, &channel_id, sender_id, message_id, parsed).await {
        Ok(reply) => reply,
        Err(error) => {
            state
                .recent_messages
                .lock()
                .await
                .release(&channel_id, message_id);
            return Err(error);
        }
    };
    send_reply(state, ReplyTarget::User(user_openid), message_id, reply).await
}

// ── 天工 Server 调用 ──────────────────────────────────────────

async fn forward_to_tiangong(
    state: &BotState,
    channel_id: &str,
    sender_id: &str,
    message_id: &str,
    parsed: ParsedMessage,
) -> Result<BotReply> {
    let url = format!(
        "{}/api/v1/messages",
        state.tiangong_url.trim_end_matches('/')
    );
    let request = ConnectorRequest {
        connector: "qq-bot".to_string(),
        channel_id: channel_id.to_string(),
        sender_id: sender_id.to_string(),
        message_id: Some(message_id.to_string()),
        message: None,
        content: Some(parsed.content),
        media: parsed.media,
    };

    let mut builder = state.http.post(url);
    if let Some(token) = &state.tiangong_token {
        builder = builder.bearer_auth(token);
    }
    let response = builder
        .json(&request)
        .send()
        .await
        .context("天工 server 请求失败")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("天工 server 错误: {status}, {body}"));
    }

    let body: ConnectorResponse = response.json().await.context("解析天工响应失败")?;
    Ok(reply_from_connector_response(body))
}

fn reply_from_connector_response(body: ConnectorResponse) -> BotReply {
    let text = {
        let content_text = body.content.text();
        if content_text.trim().is_empty() {
            body.message
        } else {
            content_text
        }
    };
    let mut files = Vec::new();
    for content in std::iter::once(&body.content).chain(body.attachments.iter()) {
        let Some(url) = content.local_file_url() else {
            continue;
        };
        let path = PathBuf::from(url.strip_prefix("file://").unwrap_or(url));
        if !path.is_absolute() {
            tracing::warn!("忽略非本地文件形式的天工附件: {url}");
            continue;
        }
        if !files.iter().any(|existing: &PathBuf| existing == &path) {
            files.push(path);
        }
    }
    BotReply { text, files }
}

// ── 回复 ──────────────────────────────────────────────────────

enum ReplyTarget {
    /// 群消息：用 group_openid
    Group(String),
    /// 私聊消息：用 user_openid
    User(String),
}

async fn send_reply(
    state: &BotState,
    target: ReplyTarget,
    msg_id: &str,
    reply: BotReply,
) -> Result<()> {
    if reply.files.is_empty() {
        if reply.text.trim().is_empty() {
            return Ok(());
        }
        send_text(state, &target, msg_id, &reply.text).await?;
        return Ok(());
    }

    // 有本地文件时，先发送文本（若有），再依次上传文件
    if !reply.text.trim().is_empty() {
        send_text(state, &target, msg_id, &reply.text).await?;
    }
    for path in &reply.files {
        let caption = "";
        tracing::info!("向 QQ 发送本地文件: {}", path.display());
        if let Err(error) = send_local_file(state, &target, path, caption).await {
            tracing::warn!("上传 QQ 本地文件失败: {error}");
        }
    }
    Ok(())
}

async fn send_text(state: &BotState, target: &ReplyTarget, msg_id: &str, text: &str) -> Result<()> {
    let access_token = state.token.get().await?;
    let url = match target {
        ReplyTarget::Group(group_openid) => {
            format!("{OPENAPI_BASE}/v2/groups/{group_openid}/messages")
        }
        ReplyTarget::User(user_openid) => {
            format!("{OPENAPI_BASE}/v2/users/{user_openid}/messages")
        }
    };
    let body = json!({
        // 0=文本，2=Markdown，7=富媒体
        "msg_type": 0,
        "msg_id": msg_id,
        "content": text,
    });
    let response = state
        .http
        .post(url)
        .header("Authorization", format!("QQBot {access_token}"))
        .json(&body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("发送 QQ 消息请求失败")?;
    check_send_response(response, "send_text").await
}

/// 上传本地文件并发送到 QQ（通过富媒体消息接口）。
///
/// QQ v2 接口要求先调用 `/v2/files/{openid}/rich_media` 上传获取 `file_info`，
/// 再以 `msg_type=7` 发送富媒体消息。
async fn send_local_file(
    state: &BotState,
    target: &ReplyTarget,
    path: &std::path::Path,
    _caption: &str,
) -> Result<()> {
    let access_token = state.token.get().await?;
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("读取待发送文件失败: {}", path.display()))?;
    let file_type = qq_file_type(path);
    let (upload_url, openid) = match target {
        ReplyTarget::Group(group_openid) => (
            format!("{OPENAPI_BASE}/v2/groups/{group_openid}/files"),
            group_openid.clone(),
        ),
        ReplyTarget::User(user_openid) => (
            format!("{OPENAPI_BASE}/v2/users/{user_openid}/files"),
            user_openid.clone(),
        ),
    };

    let file_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    let upload_body = json!({
        "file_type": file_type,
        "url": format!("data:application/octet-stream;base64,{file_data}"),
        "srv_send_msg": false,
    });
    let upload_response = state
        .http
        .post(&upload_url)
        .header("Authorization", format!("QQBot {access_token}"))
        .json(&upload_body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("上传 QQ 富媒体文件失败")?;
    let file_uuid = extract_file_uuid(upload_response).await?;

    // 以 msg_type=7 发送富媒体消息
    let send_url = match target {
        ReplyTarget::Group(_) => format!("{OPENAPI_BASE}/v2/groups/{openid}/messages"),
        ReplyTarget::User(_) => format!("{OPENAPI_BASE}/v2/users/{openid}/messages"),
    };
    let body = json!({
        "msg_type": 7,
        "msg_id": "",
        "media": {
            "file_info": file_uuid,
        },
    });
    let response = state
        .http
        .post(&send_url)
        .header("Authorization", format!("QQBot {access_token}"))
        .json(&body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("发送 QQ 富媒体消息失败")?;
    check_send_response(response, "send_rich_media").await
}

async fn extract_file_uuid(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "上传 QQ 富媒体文件失败（HTTP {status}）: {}",
            truncate_str(&body, 256)
        ));
    }
    let value: Value = serde_json::from_str(&body)
        .with_context(|| format!("解析 QQ 富媒体上传响应失败: {}", truncate_str(&body, 256)))?;
    Ok(value
        .get("file_uuid")
        .or_else(|| value.get("file_info"))
        .cloned()
        .unwrap_or(value))
}

async fn check_send_response(response: reqwest::Response, api: &str) -> Result<()> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "QQ {api} 失败（HTTP {status}）: {}",
            truncate_str(&body, 256)
        ));
    }
    // QQ OpenAPI 在 HTTP 200 下用 code 表示业务结果（0=成功）
    if let Ok(value) = serde_json::from_str::<Value>(&body)
        && let Some(code) = value.get("code").and_then(Value::as_i64)
        && code != 0
    {
        let message = value
            .get("message")
            .or_else(|| value.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(anyhow!("QQ {api} 业务失败: code={code}, message={message}"));
    }
    Ok(())
}

// ── 图片下载 ──────────────────────────────────────────────────

struct DownloadedImage {
    url: String,
    mime_type: String,
}

async fn download_attachment(
    state: &BotState,
    attachment: &QqAttachment,
) -> Result<DownloadedImage> {
    // QQ 富媒体 URL 可能需要补全 https 前缀
    let raw_url = attachment.url.trim();
    let url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
        raw_url.to_string()
    } else {
        format!("{OPENAPI_BASE}{raw_url}")
    };

    let access_token = state.token.get().await?;
    let response = state
        .http
        .get(&url)
        .header("Authorization", format!("QQBot {access_token}"))
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| anyhow!("下载 QQ 图片请求失败: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "下载 QQ 图片失败（HTTP {status}）: {}",
            truncate_str(&body, 256)
        ));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .filter(|value| value.starts_with("image/"))
        .unwrap_or("image/jpeg")
        .to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| anyhow!("读取 QQ 图片数据失败: {error}"))?;
    if bytes.is_empty() {
        return Err(anyhow!("QQ 图片下载数据为空"));
    }
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    Ok(DownloadedImage {
        url: format!("data:{content_type};base64,{encoded}"),
        mime_type: content_type,
    })
}

// ── 工具 ──────────────────────────────────────────────────────

/// 去除 QQ 群 @机器人 的前缀（QQ 群消息 content 形如 `<@!123> 实际文本`）。
fn strip_at_mention(content: &str) -> String {
    let mut remaining = content.trim();
    while remaining.starts_with('<') {
        if let Some(end) = remaining.find('>') {
            remaining = remaining[end + 1..].trim_start();
        } else {
            break;
        }
    }
    remaining.to_string()
}

fn qq_file_type(path: &std::path::Path) -> u32 {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => 1,
        "mp4" | "mov" | "avi" | "mkv" => 2,
        "mp3" | "wav" | "m4a" | "aac" | "flac" => 3,
        _ => 4,
    }
}

fn truncate_str(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_messages_deduplicates_and_releases() {
        let mut messages = RecentMessages::default();
        assert!(messages.claim("group:1", "msg-1"));
        assert!(!messages.claim("group:1", "msg-1"));
        assert!(messages.claim("c2c:2", "msg-1"));
        messages.release("group:1", "msg-1");
        assert!(messages.claim("group:1", "msg-1"));
    }

    #[test]
    fn strip_at_mention_removes_robot_prefix() {
        assert_eq!(strip_at_mention("<@!102000> 你好"), "你好");
        assert_eq!(strip_at_mention("  <@!1><@!2> 两段"), "两段");
        assert_eq!(strip_at_mention("普通消息"), "普通消息");
    }

    #[test]
    fn qq_file_type_classifies_known_extensions() {
        assert_eq!(qq_file_type(std::path::Path::new("/tmp/a.png")), 1);
        assert_eq!(qq_file_type(std::path::Path::new("/tmp/a.mp4")), 2);
        assert_eq!(qq_file_type(std::path::Path::new("/tmp/a.mp3")), 3);
        assert_eq!(qq_file_type(std::path::Path::new("/tmp/a.txt")), 4);
    }

    #[test]
    fn connector_request_preserves_message_id_and_images() {
        let request = ConnectorRequest {
            connector: "qq-bot".to_string(),
            channel_id: "group:abc".to_string(),
            sender_id: "member-1".to_string(),
            message_id: Some("msg-1".to_string()),
            message: None,
            content: Some(ApiMessageContent::Text {
                text: "你好".into(),
            }),
            media: vec![MediaAsset::image(
                "data:image/png;base64,AA==".into(),
                "image/png".into(),
            )],
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["connector"], "qq-bot");
        assert_eq!(value["channel_id"], "group:abc");
        assert_eq!(value["message_id"], "msg-1");
        assert_eq!(value["media"][0]["capability"], "multimodal");
    }

    #[test]
    fn reply_extracts_local_files_and_caption() {
        let reply = reply_from_connector_response(ConnectorResponse {
            session_id: "s".into(),
            message: "图片已生成".into(),
            content: ApiMessageContent::Image {
                url: "/tmp/image.jpg".into(),
                caption: Some("图片已生成".into()),
            },
            attachments: vec![ApiMessageContent::File {
                url: "/tmp/report.txt".into(),
                name: "report.txt".into(),
            }],
        });
        assert_eq!(reply.text, "图片已生成");
        assert_eq!(
            reply.files,
            vec![
                PathBuf::from("/tmp/image.jpg"),
                PathBuf::from("/tmp/report.txt")
            ]
        );
    }

    #[test]
    fn reply_ignores_remote_file_urls() {
        let reply = reply_from_connector_response(ConnectorResponse {
            session_id: "s".into(),
            message: "仅文本".into(),
            content: ApiMessageContent::Text {
                text: "仅文本".into(),
            },
            attachments: vec![ApiMessageContent::Image {
                url: "https://example.com/x.png".into(),
                caption: None,
            }],
        });
        assert!(reply.files.is_empty());
    }

    #[test]
    fn c2c_message_parses_attachments() {
        let raw = r#"{
            "id":"msg-9",
            "content":"看图",
            "user_openid":"user-1",
            "author":{"member_openid":"","user_openid":"user-1"},
            "attachments":[{"content_type":"image/png","url":"/path","filename":"a.png"}]
        }"#;
        let message: C2cMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(message.id, "msg-9");
        assert_eq!(message.user_openid, "user-1");
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].content_type, "image/png");
    }

    #[test]
    fn group_message_parses_basic_fields() {
        let raw = r#"{
            "id":"msg-1",
            "content":"<@!1> 你好",
            "group_openid":"group-1",
            "message_type":"group",
            "author":{"member_openid":"member-1","user_openid":""}
        }"#;
        let message: GroupAtMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(message.group_openid, "group-1");
        assert_eq!(message.author.member_openid, "member-1");
    }
}
