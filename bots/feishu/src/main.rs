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

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use openlark_client::ws_client::{EventDispatcherHandler, EventHandler, LarkWsClient};
use openlark_client::CoreConfig as Config;
use serde::{Deserialize, Serialize};
use tokio::signal;
use tokio::sync::RwLock;
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
    #[serde(rename = "messageType", alias = "message_type", alias = "msg_type", default)]
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
    let tiangong_url = std::env::var("TIANGONG_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
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
        let mut term =
            signal::unix::signal(signal::unix::SignalKind::terminate()).context("注册 SIGTERM 处理失败")?;
        let mut int =
            signal::unix::signal(signal::unix::SignalKind::interrupt()).context("注册 SIGINT 处理失败")?;
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

    // 给用户消息添加已接收标记
    let mut salute_reaction_id = String::new();
    if let Some(ref msg_id) = message_id {
        match add_reaction(state, msg_id, "SALUTE").await {
            Ok(rid) => salute_reaction_id = rid,
            Err(e) => tracing::warn!("添加接收标记失败: {e}"),
        }
    }

    let mut extra_media: Vec<MediaAsset> = Vec::new();

    // 解析消息内容 → ApiMessageContent
    let content = match msg_type.as_str() {
        "text" => {
            let text = extract_text_from_message(&envelope.event.message.content)?;
            ApiMessageContent::Text { text }
        }
        "image" => {
            let image_key = extract_image_key(&envelope.event.message.content)?;
            tracing::info!("收到图片消息 image_key={image_key}");
            let image_url = download_image_as_data_uri(state, &message_id, &image_key).await?;
            ApiMessageContent::Image {
                url: image_url,
                caption: None,
            }
        }
        "post" => {
            tracing::info!(
                "开始解析 post 消息 content={}",
                truncate_str(&envelope.event.message.content, 200)
            );
            let (text, images) = extract_post_with_images(
                &envelope.event.message.content,
                state,
                &message_id,
            )
            .await?;
            tracing::info!(
                "post 解析完成 text={} images={}",
                truncate_str(&text, 100),
                images.len()
            );
            if images.is_empty() {
                ApiMessageContent::Text { text }
            } else if text.is_empty() {
                // 只有图片没有文字，发送第一张图片
                ApiMessageContent::Image {
                    url: images.into_iter().next().unwrap(),
                    caption: None,
                }
            } else {
                // 有文字也有图片，文本放在 content，图片放在 media
                extra_media = images
                    .into_iter()
                    .map(|url| MediaAsset {
                        kind: "image".to_string(),
                        url,
                        mime_type: None,
                        title: None,
                        capability: Some("multimodal".to_string()),
                    })
                    .collect();
                ApiMessageContent::Text { text }
            }
        }
        _ => {
            tracing::warn!("不支持的消息类型: {msg_type}");
            return Ok(());
        }
    };

    let text = content.text();
    if text.is_empty() {
        return Ok(());
    }

    tracing::info!("转发到天工 chat_id={chat_id} text={}", truncate_str(&text, 100));

    // 转发到天工 server
    match forward_to_tiangong(state, chat_id, &sender_id, &message_id, content, extra_media).await {
        Ok(reply) => {
            tracing::info!("天工回复 chat_id={chat_id} len={}", reply.len());
            if let Err(e) = send_reply(state, chat_id, &reply).await {
                tracing::error!("发送回复失败: {e}");
            }
        }
        Err(e) => {
            tracing::error!("天工调用失败: {e}");
            let _ = send_reply(state, chat_id, "抱歉，处理消息时出现了错误，请稍后重试。").await;
        }
    }

    // 处理完成，切换标记：移除接收标记，添加完成标记
    if let Some(ref msg_id) = message_id {
        if !salute_reaction_id.is_empty()
            && let Err(e) = remove_reaction(state, msg_id, &salute_reaction_id).await
        {
            tracing::warn!("移除接收标记失败: {e}");
        }
        if let Err(e) = add_reaction(state, msg_id, "OK").await {
            tracing::warn!("添加完成标记失败: {e}");
        }
    }

    Ok(())
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

/// 解析 post 类型消息，提取文本和图片（图片下载为 data URI）
async fn extract_post_with_images(
    content: &str,
    state: &Arc<BotState>,
    message_id: &Option<String>,
) -> Result<(String, Vec<String>)> {
    let value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| anyhow!("解析 post 消息失败: {e}"))?;

    let mut text_parts = Vec::new();
    let mut images = Vec::new();

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

    Ok((text_parts.join(""), images))
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

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    tracing::debug!("图片下载响应 status={status} content_type={content_type}");

    if !status.is_success() {
        let error_body = resp.text().await.unwrap_or_default();
        tracing::warn!("图片下载失败 status={status} body={error_body}");
        return Ok("data:image/jpeg;base64,".to_string());
    }

    if status.as_u16() == 401 {
        // token 过期，刷新重试
        let new_token = refresh_token(state).await?;
        let retry = state
            .http
            .get(format!(
                "https://open.feishu.cn/open-apis/im/v1/messages/{msg_id}/resources/{image_key}"
            ))
            .query(&[("type", "image")])
            .bearer_auth(&new_token)
            .send()
            .await
            .map_err(|e| anyhow!("重试下载图片失败: {e}"))?;
        let bytes = retry
            .bytes()
            .await
            .map_err(|e| anyhow!("读取图片数据失败: {e}"))?;
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        return Ok(format!("data:image/jpeg;base64,{b64}"));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow!("读取图片数据失败: {e}"))?;

    tracing::info!("图片下载完成 image_key={image_key} size={}KB", bytes.len() / 1024);

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    Ok(format!("data:image/jpeg;base64,{b64}"))
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

async fn remove_reaction(
    state: &Arc<BotState>,
    message_id: &str,
    reaction_id: &str,
) -> Result<()> {
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

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}
