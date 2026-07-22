//! 天工飞书 bot 制品。
//!
//! 独立编译的二进制，由天工主程序在运行时下载并启动。
//! 通过 openlark WebSocket 长连接接收飞书消息，转发到天工本地 embedded
//! server 的 `POST /api/v1/messages`，并把回复通过飞书互动卡片发回。
//!
//! 凭证由天工主程序通过环境变量注入：
//! - `TIANGONG_BOT_FEISHU_APP_ID` / `TIANGONG_BOT_FEISHU_APP_SECRET`
//! - `TIANGONG_URL`（默认 `http://127.0.0.1:8080`）
//! - `TIANGONG_TOKEN`（可选，embedded server 认证 token）

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use openlark_client::CoreConfig as Config;
use openlark_client::ws_client::{EventDispatcherHandler, EventHandler, LarkWsClient};
use serde::{Deserialize, Serialize};
use tokio::signal;
use tokio::sync::{Mutex, RwLock};
use tracing_subscriber::EnvFilter;

mod provision;
mod schema;

// ── 数据结构 ──────────────────────────────────────────────────

struct BotState {
    http: reqwest::Client,
    app_id: String,
    app_secret: String,
    access_token: RwLock<Option<String>>,
    /// 天工 server 地址，如 http://127.0.0.1:8080
    tiangong_url: String,
    /// 天工 server 认证 token（可选）
    tiangong_token: Option<String>,
    recent_messages: Mutex<RecentMessages>,
}

const RECENT_MESSAGE_LIMIT: usize = 1024;

#[derive(Default)]
struct RecentMessages {
    keys: VecDeque<(String, String)>,
}

impl RecentMessages {
    fn claim(&mut self, channel_id: &str, message_id: &str) -> bool {
        if self
            .keys
            .iter()
            .any(|(channel, message)| channel == channel_id && message == message_id)
        {
            return false;
        }
        self.keys
            .push_back((channel_id.to_string(), message_id.to_string()));
        if self.keys.len() > RECENT_MESSAGE_LIMIT {
            self.keys.pop_front();
        }
        true
    }

    fn release(&mut self, channel_id: &str, message_id: &str) {
        self.keys
            .retain(|(channel, message)| channel != channel_id || message != message_id);
    }
}

#[derive(Debug, Deserialize)]
struct EventEnvelope {
    event: EventBody,
}

#[derive(Debug, Deserialize)]
struct EventBody {
    sender: EventSender,
    message: EventMessage,
}

#[derive(Debug, Deserialize)]
struct EventSender {
    sender_id: EventSenderId,
    #[serde(rename = "senderType", alias = "sender_type")]
    sender_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct EventSenderId {
    #[serde(rename = "openId", alias = "open_id")]
    open_id: Option<String>,
    #[serde(rename = "unionId", alias = "union_id")]
    union_id: Option<String>,
    #[serde(rename = "userId", alias = "user_id")]
    user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EventMessage {
    #[serde(rename = "chatId", alias = "chat_id")]
    chat_id: String,
    #[serde(
        rename = "messageType",
        alias = "message_type",
        alias = "msg_type",
        default
    )]
    msg_type: String,
    #[serde(default)]
    content: String,
    #[serde(rename = "messageId", alias = "message_id")]
    message_id: Option<String>,
}

// ── 天工 Server API 类型 ──────────────────────────────────────

/// POST /api/v1/messages 请求体
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

/// POST /api/v1/messages 响应体
#[derive(Debug, Deserialize)]
struct ConnectorResponse {
    #[allow(dead_code)]
    session_id: String,
    #[allow(dead_code)]
    connector: String,
    #[allow(dead_code)]
    channel_id: String,
    message: String,
    content: ApiMessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiMessageContent {
    Text {
        text: String,
    },
    Image {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    #[allow(dead_code)]
    File {
        url: String,
        name: String,
    },
    #[allow(dead_code)]
    Audio {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<u32>,
    },
    #[allow(dead_code)]
    Video {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
}

impl ApiMessageContent {
    fn text(&self) -> String {
        match self {
            Self::Text { text } => text.clone(),
            _ => String::new(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Text { text } => text.trim().is_empty(),
            Self::Image { url, .. } => url.trim().is_empty(),
            Self::File { url, name } => url.trim().is_empty() || name.trim().is_empty(),
            Self::Audio { url, .. } | Self::Video { url, .. } => url.trim().is_empty(),
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
    fn image(url: String) -> Self {
        Self {
            kind: "image".to_string(),
            url,
            mime_type: None,
            title: None,
            capability: Some("multimodal".to_string()),
        }
    }
}

struct ParsedMessage {
    content: ApiMessageContent,
    media: Vec<MediaAsset>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageHandling {
    Forwarded,
    ReplyFailed,
    Ignored,
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
            install_rustls_provider()?;
            println!("{}", serde_json::to_string(&provision::begin().await?)?);
            return Ok(());
        }
        Some("--provision-poll") => {
            install_rustls_provider()?;
            println!(
                "{}",
                serde_json::to_string(&provision::poll_from_stdin().await?)?
            );
            return Ok(());
        }
        _ => {}
    }

    // 初始化日志（输出到 stderr，主程序捕获 tail）
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(std::io::stderr)
        .init();

    // 安装 rustls 加密后端
    install_rustls_provider()?;

    let credentials = provision::load_credentials()?;
    let app_id = credentials.app_id;
    let app_secret = credentials.app_secret;
    let tiangong_url =
        std::env::var("TIANGONG_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    let tiangong_token = std::env::var("TIANGONG_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());

    tracing::info!("飞书机器人启动中...");
    tracing::info!("天工服务地址: {tiangong_url}");

    let state = Arc::new(BotState {
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?,
        app_id,
        app_secret,
        access_token: RwLock::new(None),
        tiangong_url,
        tiangong_token,
        recent_messages: Mutex::new(RecentMessages::default()),
    });

    // 注册事件处理器
    let handler = FeishuHandler {
        state: state.clone(),
    };
    let dispatcher = EventDispatcherHandler::builder()
        .register_raw("im.message.receive_v1", handler)
        .map_err(|e| anyhow!("注册事件处理器失败: {e}"))?
        .build();

    // 构建配置
    let config = Config::builder()
        .app_id(&state.app_id)
        .app_secret(&state.app_secret)
        .build();

    tracing::info!("正在连接飞书 WebSocket 长连接...");

    // 在独立 task 启动 WebSocket 长连接
    let ws_handle = tokio::spawn({
        let config = Arc::new(config);
        async move {
            if let Err(e) = LarkWsClient::open(config, dispatcher).await {
                tracing::error!("飞书 WebSocket 长连接退出: {e}");
            }
        }
    });

    // 等待终止信号（SIGTERM/SIGINT/Ctrl+C），主程序 stop 时发送
    #[cfg(unix)]
    {
        let mut term = signal::unix::signal(signal::unix::SignalKind::terminate())
            .context("注册 SIGTERM 处理失败")?;
        let mut int = signal::unix::signal(signal::unix::SignalKind::interrupt())
            .context("注册 SIGINT 处理失败")?;
        tokio::select! {
            _ = term.recv() => tracing::info!("收到 SIGTERM，正在退出..."),
            _ = int.recv() => tracing::info!("收到 SIGINT，正在退出..."),
            _ = ws_handle => tracing::info!("WebSocket 长连接已结束"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = signal::ctrl_c() => tracing::info!("收到 Ctrl+C，正在退出..."),
            _ = ws_handle => tracing::info!("WebSocket 长连接已结束"),
        }
    }

    Ok(())
}

fn install_rustls_provider() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|e| anyhow!("安装 rustls provider 失败: {e:?}"))
}

// ── 事件处理器 ────────────────────────────────────────────────

struct FeishuHandler {
    state: Arc<BotState>,
}

impl EventHandler for FeishuHandler {
    fn handle(&self, event: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.clone();
        let payload = event.to_vec();

        tokio::spawn(async move {
            if let Err(e) = handle_event(&state, &payload).await {
                tracing::error!("处理飞书消息失败: {e}");
            }
        });

        Ok(())
    }
}

// ── 事件处理 ──────────────────────────────────────────────────

async fn handle_event(state: &Arc<BotState>, payload: &[u8]) -> Result<()> {
    let raw = String::from_utf8_lossy(payload);
    let envelope: EventEnvelope = serde_json::from_str(&raw).map_err(|e| {
        tracing::error!("解析事件 JSON 失败: {e}\n原始数据: {raw}");
        anyhow!("解析事件 JSON 失败: {e}")
    })?;

    // 只处理用户消息，忽略机器人自身消息
    if envelope.event.sender.sender_type.as_deref() != Some("user") {
        return Ok(());
    }

    let chat_id = &envelope.event.message.chat_id;
    let msg_type = &envelope.event.message.msg_type;
    let message_id = envelope.event.message.message_id.clone();
    let sender_id = envelope
        .event
        .sender
        .sender_id
        .open_id
        .clone()
        .or_else(|| envelope.event.sender.sender_id.user_id.clone())
        .unwrap_or_default();

    tracing::info!("收到飞书消息 chat_id={chat_id} msg_type={msg_type} sender={sender_id}");

    // 如果 msg_type 为空，打印原始 JSON 用于调试
    if msg_type.is_empty() {
        tracing::warn!("msg_type 为空，原始事件数据:\n{raw}");
    }

    if let Some(message_id) = message_id.as_deref()
        && !state
            .recent_messages
            .lock()
            .await
            .claim(chat_id, message_id)
    {
        tracing::info!("忽略重复飞书消息 chat_id={chat_id} message_id={message_id}");
        return Ok(());
    }

    // 给用户消息添加已接收标记
    let mut salute_reaction_id = String::new();
    if let Some(ref msg_id) = message_id {
        match add_reaction(state, msg_id, "SALUTE").await {
            Ok(rid) => salute_reaction_id = rid,
            Err(e) => tracing::warn!("添加接收标记失败: {e}"),
        }
    }

    let handling = process_user_message(
        state,
        &envelope.event.message,
        chat_id,
        &sender_id,
        &message_id,
    )
    .await;

    let retryable_failure = handling.is_err();
    if let Err(error) = &handling {
        tracing::error!("飞书消息处理失败: {error}");
        if let Err(reply_error) =
            send_reply(state, chat_id, "抱歉，处理消息时出现了错误，请稍后重试。").await
        {
            tracing::warn!("发送失败提示失败: {reply_error}");
        }
    }

    finish_message_reaction(
        state,
        message_id.as_deref(),
        &salute_reaction_id,
        matches!(&handling, Ok(MessageHandling::Forwarded)),
    )
    .await;

    if retryable_failure && let Some(message_id) = message_id.as_deref() {
        state
            .recent_messages
            .lock()
            .await
            .release(chat_id, message_id);
    }

    handling.map(|_| ())
}

async fn process_user_message(
    state: &Arc<BotState>,
    message: &EventMessage,
    chat_id: &str,
    sender_id: &str,
    message_id: &Option<String>,
) -> Result<MessageHandling> {
    let parsed = match message.msg_type.as_str() {
        "text" => {
            let text = extract_text_from_message(&message.content)?;
            ParsedMessage {
                content: ApiMessageContent::Text { text },
                media: Vec::new(),
            }
        }
        "image" => {
            let image_key = extract_image_key(&message.content)?;
            tracing::info!("收到图片消息 image_key={image_key}");
            let image_url = download_image_as_data_uri(state, message_id, &image_key).await?;
            ParsedMessage {
                content: ApiMessageContent::Image {
                    url: image_url,
                    caption: None,
                },
                media: Vec::new(),
            }
        }
        "post" => {
            tracing::info!(
                "开始解析 post 消息 content={}",
                truncate_str(&message.content, 200)
            );
            let (text, images, failed_images) =
                extract_post_with_images(&message.content, state, message_id).await?;
            tracing::info!(
                "post 解析完成 text={} images={} failed_images={failed_images}",
                truncate_str(&text, 100),
                images.len()
            );
            if text.trim().is_empty() && images.is_empty() && failed_images > 0 {
                return Err(anyhow!("post 消息中的图片全部下载失败"));
            }
            compose_post_message(text, images)
        }
        _ => {
            tracing::warn!("不支持的消息类型: {}", message.msg_type);
            return Ok(MessageHandling::Ignored);
        }
    };

    if parsed.content.is_empty() {
        tracing::info!("忽略空消息 msg_type={}", message.msg_type);
        return Ok(MessageHandling::Ignored);
    }

    tracing::info!(
        "转发到天工 chat_id={chat_id} content_type={} media={}",
        parsed.content.kind(),
        parsed.media.len()
    );

    match forward_to_tiangong(
        state,
        chat_id,
        sender_id,
        message_id,
        parsed.content,
        parsed.media,
    )
    .await
    {
        Ok(reply) => {
            tracing::info!("天工回复 chat_id={chat_id} len={}", reply.len());
            match send_reply(state, chat_id, &reply).await {
                Ok(()) => Ok(MessageHandling::Forwarded),
                Err(error) => {
                    tracing::error!("发送飞书回复失败: {error}");
                    Ok(MessageHandling::ReplyFailed)
                }
            }
        }
        Err(error) => {
            tracing::error!("天工调用失败: {error}");
            Err(error)
        }
    }
}

async fn finish_message_reaction(
    state: &Arc<BotState>,
    message_id: Option<&str>,
    salute_reaction_id: &str,
    completed: bool,
) {
    if let Some(message_id) = message_id {
        if !salute_reaction_id.is_empty()
            && let Err(error) = remove_reaction(state, message_id, salute_reaction_id).await
        {
            tracing::warn!("移除接收标记失败: {error}");
        }
        if completed && let Err(error) = add_reaction(state, message_id, "OK").await {
            tracing::warn!("添加完成标记失败: {error}");
        }
    }
}

// ── 消息解析 ──────────────────────────────────────────────────

fn extract_text_from_message(content: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| anyhow!("解析消息 content 失败: {e}"))?;

    // text 类型: {"text": "xxx"}
    if let Some(text) = value["text"].as_str() {
        return Ok(text.to_string());
    }

    Ok(value.to_string())
}

fn extract_image_key(content: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| anyhow!("解析图片消息失败: {e}"))?;

    let key = value["image_key"]
        .as_str()
        .ok_or_else(|| anyhow!("图片消息缺少 image_key"))?;

    Ok(key.to_string())
}

fn compose_post_message(text: String, images: Vec<String>) -> ParsedMessage {
    if images.is_empty() {
        return ParsedMessage {
            content: ApiMessageContent::Text { text },
            media: Vec::new(),
        };
    }

    if text.trim().is_empty() {
        let mut images = images.into_iter();
        let first_image = images.next().expect("已确认富文本至少包含一张图片");
        return ParsedMessage {
            content: ApiMessageContent::Image {
                url: first_image,
                caption: None,
            },
            media: images.map(MediaAsset::image).collect(),
        };
    }

    ParsedMessage {
        content: ApiMessageContent::Text { text },
        media: images.into_iter().map(MediaAsset::image).collect(),
    }
}

/// 解析 post 类型消息，提取文本和图片（图片下载为 data URI）
async fn extract_post_with_images(
    content: &str,
    state: &Arc<BotState>,
    message_id: &Option<String>,
) -> Result<(String, Vec<String>, usize)> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| anyhow!("解析 post 消息失败: {e}"))?;

    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    let mut failed_images = 0;

    // 飞书 post 有两种格式：
    // 1. 扁平格式: {"title":"","content":[[...]]}
    // 2. 多语言格式: {"zh_cn":{"title":"","content":[[...]]}}
    let paragraphs = if let Some(arr) = value["content"].as_array() {
        tracing::debug!("post 扁平格式，content 数组长度={}", arr.len());
        arr.clone()
    } else {
        // 多语言格式，遍历语言键
        tracing::debug!(
            "post 尝试多语言格式，顶层 keys={:?}",
            value.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
        let mut result = vec![];
        if let Some(obj) = value.as_object() {
            for (lang, body) in obj {
                if let Some(arr) = body["content"].as_array() {
                    tracing::debug!("post 语言={lang} content 数组长度={}", arr.len());
                    result.extend(arr.clone());
                }
            }
        }
        result
    };

    tracing::debug!("post paragraphs 数量={}", paragraphs.len());

    for (pi, para) in paragraphs.iter().enumerate() {
        if let Some(elems) = para.as_array() {
            tracing::debug!("post paragraph[{pi}] elements={}", elems.len());
            for (ei, elem) in elems.iter().enumerate() {
                let tag = elem["tag"].as_str().unwrap_or("");
                tracing::debug!("post paragraph[{pi}][{ei}] tag={tag}");
                match tag {
                    "text" => {
                        if let Some(t) = elem["text"].as_str() {
                            text_parts.push(t.to_string());
                        }
                    }
                    "at" => {
                        if let Some(uid) = elem["user_id"].as_str() {
                            text_parts.push(format!("@{uid}"));
                        }
                    }
                    "img" => {
                        if let Some(key) = elem["image_key"].as_str() {
                            tracing::info!("下载 post 内嵌图片 image_key={key}");
                            match download_image_as_data_uri(state, message_id, key).await {
                                Ok(url) => images.push(url),
                                Err(e) => {
                                    failed_images += 1;
                                    tracing::warn!("下载图片失败 key={key}: {e}");
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Ok((text_parts.join(""), images, failed_images))
}

// ── 图片下载 ──────────────────────────────────────────────────

async fn download_image_as_data_uri(
    state: &Arc<BotState>,
    message_id: &Option<String>,
    image_key: &str,
) -> Result<String> {
    let token = get_token(state).await?;
    let msg_id = message_id
        .as_deref()
        .ok_or_else(|| anyhow!("缺少 message_id，无法下载图片"))?;

    let resp = state
        .http
        .get(format!(
            "https://open.feishu.cn/open-apis/im/v1/messages/{msg_id}/resources/{image_key}"
        ))
        .query(&[("type", "image")])
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| anyhow!("下载图片请求失败: {e}"))?;

    let resp = if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        // token 过期，刷新重试
        let new_token = refresh_token(state).await?;
        state
            .http
            .get(format!(
                "https://open.feishu.cn/open-apis/im/v1/messages/{msg_id}/resources/{image_key}"
            ))
            .query(&[("type", "image")])
            .bearer_auth(&new_token)
            .send()
            .await
            .map_err(|e| anyhow!("重试下载图片失败: {e}"))?
    } else {
        resp
    };

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .filter(|value| value.starts_with("image/"))
        .unwrap_or("image/jpeg")
        .to_string();
    tracing::debug!("图片下载响应 status={status} content_type={content_type}");

    if !status.is_success() {
        let error_body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "图片下载失败 status={status} body={}",
            truncate_str(&error_body, 512)
        ));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow!("读取图片数据失败: {e}"))?;
    if bytes.is_empty() {
        return Err(anyhow!("图片下载结果为空 image_key={image_key}"));
    }

    tracing::info!(
        "图片下载完成 image_key={image_key} size={}KB",
        bytes.len() / 1024
    );

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    Ok(format!("data:{content_type};base64,{b64}"))
}

// ── 天工 Server 调用 ──────────────────────────────────────────

async fn forward_to_tiangong(
    state: &BotState,
    chat_id: &str,
    sender_id: &str,
    message_id: &Option<String>,
    content: ApiMessageContent,
    media: Vec<MediaAsset>,
) -> Result<String> {
    let url = format!(
        "{}/api/v1/messages",
        state.tiangong_url.trim_end_matches('/')
    );

    let req_body = ConnectorRequest {
        connector: "feishu-bot".to_string(),
        channel_id: chat_id.to_string(),
        sender_id: sender_id.to_string(),
        message_id: message_id.clone(),
        message: None,
        content: Some(content),
        media,
    };

    let mut req = state.http.post(&url);
    if let Some(ref token) = state.tiangong_token {
        req = req.bearer_auth(token);
    }

    let resp = req
        .json(&req_body)
        .send()
        .await
        .map_err(|e| anyhow!("天工 server 请求失败: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("天工 server 错误: {status}, {body}"));
    }

    let body: ConnectorResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("解析天工响应失败: {e}"))?;

    // 优先使用 content.text()，回退到 message 字段
    let reply = body.content.text();
    if reply.is_empty() {
        Ok(body.message)
    } else {
        Ok(reply)
    }
}

// ── 飞书 token 管理 ───────────────────────────────────────────

async fn refresh_token(state: &Arc<BotState>) -> Result<String> {
    let resp = state
        .http
        .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
        .json(&serde_json::json!({
            "app_id": &state.app_id,
            "app_secret": &state.app_secret,
        }))
        .send()
        .await
        .map_err(|e| anyhow!("获取 token 请求失败: {e}"))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("解析 token 响应失败: {e}"))?;

    let code = body["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = body["msg"].as_str().unwrap_or("未知错误");
        return Err(anyhow!("获取 token 失败: code={code}, msg={msg}"));
    }

    let token = body["tenant_access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("响应缺少 tenant_access_token"))?
        .to_string();

    *state.access_token.write().await = Some(token.clone());
    tracing::info!("飞书 access_token 已刷新");
    Ok(token)
}

async fn get_token(state: &Arc<BotState>) -> Result<String> {
    if let Some(t) = state.access_token.read().await.as_ref() {
        return Ok(t.clone());
    }
    refresh_token(state).await
}

// ── 发送飞书消息 ──────────────────────────────────────────────

async fn send_reply(state: &Arc<BotState>, chat_id: &str, text: &str) -> Result<()> {
    let token = get_token(state).await?;

    // 使用飞书互动卡片消息（白色背景），支持 Markdown 渲染
    let card_content = serde_json::json!({
        "config": {
            "wide_screen_mode": true
        },
        "elements": [
            {
                "tag": "markdown",
                "content": text
            }
        ]
    })
    .to_string();

    let resp = state
        .http
        .post("https://open.feishu.cn/open-apis/im/v1/messages")
        .bearer_auth(&token)
        .query(&[("receive_id_type", "chat_id")])
        .json(&serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "interactive",
            "content": card_content,
        }))
        .send()
        .await
        .map_err(|e| anyhow!("发送消息请求失败: {e}"))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("解析发送响应失败: {e}"))?;

    let code = body["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        // token 过期 → 刷新重试
        if code == 99991663 || code == 99991668 {
            tracing::warn!("token 过期，刷新后重试...");
            let new_token = refresh_token(state).await?;
            let retry = state
                .http
                .post("https://open.feishu.cn/open-apis/im/v1/messages")
                .bearer_auth(&new_token)
                .query(&[("receive_id_type", "chat_id")])
                .json(&serde_json::json!({
                    "receive_id": chat_id,
                    "msg_type": "interactive",
                    "content": card_content,
                }))
                .send()
                .await
                .map_err(|e| anyhow!("重试发送失败: {e}"))?;

            let retry_body: serde_json::Value = retry
                .json()
                .await
                .map_err(|e| anyhow!("解析重试响应失败: {e}"))?;
            let rc = retry_body["code"].as_i64().unwrap_or(-1);
            if rc != 0 {
                let rm = retry_body["msg"].as_str().unwrap_or("未知错误");
                return Err(anyhow!("重试发送失败: code={rc}, msg={rm}"));
            }
        } else {
            let msg = body["msg"].as_str().unwrap_or("未知错误");
            return Err(anyhow!("发送消息失败: code={code}, msg={msg}"));
        }
    }
    Ok(())
}

// ── 消息表情回应 ──────────────────────────────────────────────

async fn add_reaction(state: &Arc<BotState>, message_id: &str, emoji: &str) -> Result<String> {
    let token = get_token(state).await?;
    let url = format!("https://open.feishu.cn/open-apis/im/v1/messages/{message_id}/reactions");

    let resp = state
        .http
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "reaction_type": { "emoji_type": emoji }
        }))
        .send()
        .await
        .map_err(|e| anyhow!("添加表情请求失败: {e}"))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("解析表情响应失败: {e}"))?;

    let code = body["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = body["msg"].as_str().unwrap_or("未知错误");
        return Err(anyhow!("添加表情失败: code={code}, msg={msg}"));
    }

    // 返回 reaction_id
    let reaction_id = body["data"]["reaction_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    Ok(reaction_id)
}

async fn remove_reaction(state: &Arc<BotState>, message_id: &str, reaction_id: &str) -> Result<()> {
    if reaction_id.is_empty() {
        return Ok(());
    }
    let token = get_token(state).await?;
    let url = format!(
        "https://open.feishu.cn/open-apis/im/v1/messages/{message_id}/reactions/{reaction_id}"
    );

    let resp = state
        .http
        .delete(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| anyhow!("移除表情请求失败: {e}"))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("解析表情响应失败: {e}"))?;

    let code = body["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        let msg = body["msg"].as_str().unwrap_or("未知错误");
        return Err(anyhow!("移除表情失败: code={code}, msg={msg}"));
    }

    Ok(())
}

// ── 工具函数 ──────────────────────────────────────────────────

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

    fn request_from(parsed: ParsedMessage) -> ConnectorRequest {
        ConnectorRequest {
            connector: "feishu-bot".to_string(),
            channel_id: "chat".to_string(),
            sender_id: "sender".to_string(),
            message_id: Some("message".to_string()),
            message: None,
            content: Some(parsed.content),
            media: parsed.media,
        }
    }

    #[test]
    fn content_validity_depends_on_its_resource() {
        assert!(ApiMessageContent::Text { text: "  ".into() }.is_empty());
        assert!(
            ApiMessageContent::Image {
                url: String::new(),
                caption: None,
            }
            .is_empty()
        );
        assert!(
            ApiMessageContent::File {
                url: "data:file".into(),
                name: String::new(),
            }
            .is_empty()
        );
        assert!(
            !ApiMessageContent::Image {
                url: "data:image/png;base64,AA==".into(),
                caption: None,
            }
            .is_empty()
        );
    }

    #[test]
    fn single_image_is_sent_as_structured_content() {
        let parsed = ParsedMessage {
            content: ApiMessageContent::Image {
                url: "data:image/png;base64,first".into(),
                caption: None,
            },
            media: Vec::new(),
        };
        let value = serde_json::to_value(request_from(parsed)).unwrap();

        assert_eq!(value["content"]["type"], "image");
        assert_eq!(value["content"]["url"], "data:image/png;base64,first");
        assert!(value.get("media").is_none());
    }

    #[test]
    fn image_only_post_keeps_every_image_in_order() {
        let parsed = compose_post_message(
            String::new(),
            vec!["first".into(), "second".into(), "third".into()],
        );
        let value = serde_json::to_value(request_from(parsed)).unwrap();

        assert_eq!(value["content"]["type"], "image");
        assert_eq!(value["content"]["url"], "first");
        assert_eq!(value["media"].as_array().unwrap().len(), 2);
        assert_eq!(value["media"][0]["url"], "second");
        assert_eq!(value["media"][1]["url"], "third");
    }

    #[test]
    fn text_and_images_post_keeps_text_and_every_image() {
        let parsed = compose_post_message("正文".into(), vec!["first".into(), "second".into()]);
        let value = serde_json::to_value(request_from(parsed)).unwrap();

        assert_eq!(value["content"]["type"], "text");
        assert_eq!(value["content"]["text"], "正文");
        assert_eq!(value["media"].as_array().unwrap().len(), 2);
        assert_eq!(value["media"][0]["url"], "first");
        assert_eq!(value["media"][1]["url"], "second");
    }

    #[test]
    fn unicode_log_truncation_does_not_split_characters() {
        assert_eq!(truncate_str("飞书图片消息", 4), "飞书图片...");
        assert_eq!(truncate_str("飞书", 4), "飞书");
    }

    #[test]
    fn duplicate_message_claim_is_scoped_by_channel_and_releasable() {
        let mut messages = RecentMessages::default();
        assert!(messages.claim("chat-a", "message-1"));
        assert!(!messages.claim("chat-a", "message-1"));
        assert!(messages.claim("chat-b", "message-1"));

        messages.release("chat-a", "message-1");
        assert!(messages.claim("chat-a", "message-1"));
    }
}
