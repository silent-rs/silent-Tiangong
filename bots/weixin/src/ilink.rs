//! 腾讯微信 iLink Bot HTTP API 客户端。

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ILINK_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const MEDIA_CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(40);
const QR_LONG_POLL_TIMEOUT: Duration = Duration::from_secs(35);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub const DEFAULT_QR_POLL_INTERVAL: u64 = 1;
pub const DEFAULT_QR_EXPIRES_IN: i64 = 300;
const BOT_TYPE: u32 = 3;

// 与腾讯公开的 openclaw-weixin v2.4.6 协议标识保持一致。
const ILINK_APP_ID: &str = "bot";
const ILINK_APP_CLIENT_VERSION: &str = "132102";
const CHANNEL_VERSION: &str = "2.4.6";
const BOT_AGENT: &str = "Tiangong/0.1.0";

fn build_common_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("ilink-app-id", HeaderValue::from_static(ILINK_APP_ID));
    headers.insert(
        "ilink-app-clientversion",
        HeaderValue::from_static(ILINK_APP_CLIENT_VERSION),
    );
    headers
}

fn build_headers(bot_token: Option<&str>) -> Result<HeaderMap> {
    let mut headers = build_common_headers();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "authorizationtype",
        HeaderValue::from_static("ilink_bot_token"),
    );

    let random_uin: u32 = rand::random();
    let uin = base64::engine::general_purpose::STANDARD.encode(random_uin.to_string());
    headers.insert(
        "x-wechat-uin",
        HeaderValue::from_str(&uin).context("构造 X-WECHAT-UIN 失败")?,
    );

    if let Some(token) = bot_token.map(str::trim).filter(|token| !token.is_empty()) {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).context("构造 Authorization 失败")?,
        );
    }

    Ok(headers)
}

#[derive(Debug, Serialize)]
struct BaseInfo {
    channel_version: &'static str,
    bot_agent: &'static str,
}

fn base_info() -> BaseInfo {
    BaseInfo {
        channel_version: CHANNEL_VERSION,
        bot_agent: BOT_AGENT,
    }
}

pub fn normalize_base_url(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    let url = Url::parse(value).context("解析 iLink API 地址失败")?;
    if url.scheme() != "https" || url.host_str().is_none() {
        bail!("iLink API 地址必须是有效的 HTTPS 地址");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("iLink API 地址不能包含认证信息、查询参数或片段");
    }
    Ok(value.to_string())
}

pub fn redirect_base_url(redirect_host: &str) -> Result<String> {
    let redirect_host = redirect_host.trim();
    if redirect_host.is_empty() {
        bail!("iLink 扫码重定向缺少目标地址");
    }
    let value = if redirect_host.starts_with("https://") {
        redirect_host.to_string()
    } else {
        format!("https://{redirect_host}")
    };
    normalize_base_url(&value)
}

fn api_url(base_url: &str, path: &str) -> Result<Url> {
    let base = normalize_base_url(base_url)?;
    Url::parse(&format!("{base}/"))
        .context("解析 iLink API 地址失败")?
        .join(path.trim_start_matches('/'))
        .context("拼接 iLink API 地址失败")
}

// -- 扫码登录 --------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct QrCodeResponse {
    #[serde(default)]
    pub qrcode: String,
    #[serde(default)]
    pub qrcode_img_content: Option<String>,
    #[serde(default)]
    pub qrcode_url: Option<String>,
}

impl QrCodeResponse {
    pub fn render_content(&self) -> String {
        if let Some(content) = self
            .qrcode_img_content
            .as_deref()
            .map(str::trim)
            .filter(|content| !content.is_empty())
        {
            return content.to_string();
        }
        if let Some(url) = self
            .qrcode_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            return url.to_string();
        }
        self.qrcode.clone()
    }
}

#[derive(Debug, Deserialize)]
pub struct QrStatusResponse {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub baseurl: Option<String>,
    #[serde(default)]
    pub redirect_host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrStatus {
    Waiting,
    Scanned,
    Confirmed,
    Expired,
    Canceled,
    Redirect,
    NeedVerifyCode,
    VerifyCodeBlocked,
    AlreadyBound,
    Unknown,
}

impl QrStatusResponse {
    pub fn status(&self) -> QrStatus {
        match self.status.to_ascii_lowercase().as_str() {
            "wait" | "waiting" | "pending" | "" => QrStatus::Waiting,
            "scaned" | "scanned" | "scanned_confirm" => QrStatus::Scanned,
            "confirmed" | "success" | "ok" => QrStatus::Confirmed,
            "expired" | "timeout" => QrStatus::Expired,
            "canceled" | "cancelled" | "denied" => QrStatus::Canceled,
            "scaned_but_redirect" => QrStatus::Redirect,
            "need_verifycode" => QrStatus::NeedVerifyCode,
            "verify_code_blocked" => QrStatus::VerifyCodeBlocked,
            "binded_redirect" => QrStatus::AlreadyBound,
            _ => QrStatus::Unknown,
        }
    }
}

#[derive(Serialize)]
struct GetQrCodeRequest {
    local_token_list: Vec<String>,
}

pub async fn get_bot_qrcode(client: &Client, local_tokens: &[String]) -> Result<QrCodeResponse> {
    let mut url = api_url(ILINK_BASE_URL, "ilink/bot/get_bot_qrcode")?;
    url.query_pairs_mut()
        .append_pair("bot_type", &BOT_TYPE.to_string());
    let local_token_list = local_tokens
        .iter()
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .take(10)
        .map(str::to_string)
        .collect();

    let response = client
        .post(url)
        .headers(build_headers(None)?)
        .json(&GetQrCodeRequest { local_token_list })
        .timeout(QR_LONG_POLL_TIMEOUT)
        .send()
        .await
        .context("请求 iLink 二维码失败")?;
    let body = parse_body(response).await?;
    check_ret(&body, "get_bot_qrcode")?;
    serde_json::from_value(body.clone())
        .with_context(|| format!("解析 iLink 二维码响应失败: {}", truncate_json(&body)))
}

/// 返回 `None` 表示一次正常的客户端长轮询超时，调用方应继续等待。
pub async fn get_qrcode_status(
    client: &Client,
    base_url: &str,
    qrcode: &str,
) -> Result<Option<QrStatusResponse>> {
    let mut url = api_url(base_url, "ilink/bot/get_qrcode_status")?;
    url.query_pairs_mut().append_pair("qrcode", qrcode);

    let response = match client
        .get(url)
        .headers(build_common_headers())
        .timeout(QR_LONG_POLL_TIMEOUT)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) if error.is_timeout() => return Ok(None),
        Err(error) => return Err(error).context("请求 iLink 扫码状态失败"),
    };
    let body = parse_body(response).await?;
    check_ret(&body, "get_qrcode_status")?;
    serde_json::from_value(body.clone())
        .map(Some)
        .with_context(|| format!("解析 iLink 扫码状态响应失败: {}", truncate_json(&body)))
}

// -- 消息协议 --------------------------------------------------

#[derive(Debug, Serialize)]
struct GetUpdatesRequest {
    get_updates_buf: String,
    base_info: BaseInfo,
}

#[derive(Debug, Deserialize)]
pub struct UpdatesResponse {
    #[serde(default)]
    pub get_updates_buf: Option<String>,
    #[serde(default)]
    pub longpolling_timeout_ms: Option<u64>,
    #[serde(default)]
    pub msgs: Vec<WeixinMessage>,
}

#[derive(Debug, Deserialize)]
pub struct WeixinMessage {
    #[serde(default)]
    pub seq: Option<u64>,
    #[serde(default)]
    pub message_id: Option<u64>,
    #[serde(default)]
    pub from_user_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub to_user_id: String,
    #[serde(default)]
    pub message_type: i64,
    #[serde(default)]
    pub context_token: String,
    #[serde(default)]
    pub item_list: Vec<MessageItem>,
    #[serde(default)]
    pub group_id: String,
}

#[derive(Debug, Deserialize)]
pub struct MessageItem {
    #[serde(default, rename = "type")]
    pub kind: i64,
    #[serde(default)]
    pub msg_id: Option<String>,
    #[serde(default)]
    pub text_item: Option<TextItem>,
    #[serde(default)]
    pub image_item: Option<ImageItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextItem {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct CdnMedia {
    #[serde(default)]
    pub encrypt_query_param: Option<String>,
    #[serde(default)]
    pub aes_key: Option<String>,
    #[serde(default)]
    pub full_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ImageItem {
    #[serde(default)]
    pub media: Option<CdnMedia>,
    #[serde(default)]
    pub aeskey: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

impl WeixinMessage {
    pub fn is_from_user(&self) -> bool {
        self.message_type == 1
    }

    pub fn text_content(&self) -> String {
        self.item_list
            .iter()
            .filter(|item| item.kind == 1)
            .filter_map(|item| item.text_item.as_ref())
            .map(|item| item.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn images(&self) -> impl Iterator<Item = &ImageItem> {
        self.item_list
            .iter()
            .filter(|item| item.kind == 2)
            .filter_map(|item| item.image_item.as_ref())
    }

    pub fn stable_message_id(&self) -> Option<String> {
        self.message_id
            .map(|id| id.to_string())
            .or_else(|| {
                self.item_list
                    .iter()
                    .find_map(|item| item.msg_id.as_deref())
                    .map(str::to_string)
            })
            .or_else(|| self.seq.map(|seq| format!("seq-{seq}")))
    }
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    msg: OutboundMessage,
    base_info: BaseInfo,
}

#[derive(Debug, Serialize)]
struct OutboundMessage {
    from_user_id: String,
    to_user_id: String,
    client_id: String,
    message_type: i64,
    message_state: i64,
    context_token: String,
    item_list: Vec<OutboundItem>,
}

#[derive(Debug, Serialize)]
struct OutboundItem {
    #[serde(rename = "type")]
    kind: i64,
    text_item: TextItem,
}

pub async fn send_message(
    client: &Client,
    base_url: &str,
    bot_token: &str,
    to_user_id: &str,
    context_token: &str,
    text: &str,
) -> Result<()> {
    let url = api_url(base_url, "ilink/bot/sendmessage")?;
    let request = SendMessageRequest {
        msg: OutboundMessage {
            from_user_id: String::new(),
            to_user_id: to_user_id.to_string(),
            client_id: scru128::new().to_string(),
            message_type: 2,
            message_state: 2,
            context_token: context_token.to_string(),
            item_list: vec![OutboundItem {
                kind: 1,
                text_item: TextItem {
                    text: text.to_string(),
                },
            }],
        },
        base_info: base_info(),
    };

    let response = client
        .post(url)
        .headers(build_headers(Some(bot_token))?)
        .json(&request)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("发送微信消息请求失败")?;
    let body = parse_body(response).await?;
    check_ret(&body, "sendmessage")
}

pub async fn get_updates(
    client: &Client,
    base_url: &str,
    bot_token: &str,
    cursor: &str,
) -> Result<UpdatesResponse> {
    let url = api_url(base_url, "ilink/bot/getupdates")?;
    let request = GetUpdatesRequest {
        get_updates_buf: cursor.to_string(),
        base_info: base_info(),
    };
    let response = client
        .post(url)
        .headers(build_headers(Some(bot_token))?)
        .json(&request)
        .timeout(LONG_POLL_TIMEOUT)
        .send()
        .await
        .context("iLink 长轮询请求失败")?;
    let body = parse_body(response).await?;
    check_ret(&body, "getupdates")?;
    serde_json::from_value(body.clone())
        .with_context(|| format!("解析 iLink 消息响应失败: {}", truncate_json(&body)))
}

pub async fn download_image_bytes(client: &Client, image: &ImageItem) -> Result<Vec<u8>> {
    let url = build_media_download_url(image)?;
    let response = client
        .get(url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("下载微信图片请求失败")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("下载微信图片失败: {status}, {}", truncate_str(&body, 256));
    }
    response
        .bytes()
        .await
        .map(Vec::from)
        .context("读取微信图片数据失败")
}

fn build_media_download_url(image: &ImageItem) -> Result<Url> {
    let media = image.media.as_ref();
    if let Some(full_url) = media
        .and_then(|media| media.full_url.as_deref())
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .or_else(|| {
            image
                .url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
        })
    {
        let url = Url::parse(full_url).context("解析微信图片下载地址失败")?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("微信图片下载地址协议无效");
        }
        return Ok(url);
    }

    let parameter = media
        .and_then(|media| media.encrypt_query_param.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("微信图片缺少下载参数")?;
    let mut url = Url::parse(&format!("{MEDIA_CDN_BASE_URL}/download"))
        .context("解析微信媒体 CDN 地址失败")?;
    url.query_pairs_mut()
        .append_pair("encrypted_query_param", parameter);
    Ok(url)
}

#[derive(Serialize)]
struct NotificationRequest {
    base_info: BaseInfo,
}

pub async fn notify_start(client: &Client, base_url: &str, bot_token: &str) -> Result<()> {
    notify(client, base_url, bot_token, "ilink/bot/msg/notifystart").await
}

pub async fn notify_stop(client: &Client, base_url: &str, bot_token: &str) -> Result<()> {
    notify(client, base_url, bot_token, "ilink/bot/msg/notifystop").await
}

async fn notify(client: &Client, base_url: &str, bot_token: &str, path: &str) -> Result<()> {
    let response = client
        .post(api_url(base_url, path)?)
        .headers(build_headers(Some(bot_token))?)
        .json(&NotificationRequest {
            base_info: base_info(),
        })
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("通知 iLink 运行状态失败")?;
    let body = parse_body(response).await?;
    check_ret(&body, path)
}

// -- 响应处理 --------------------------------------------------

async fn parse_body(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let bytes = response.bytes().await.context("读取 iLink 响应失败")?;
    if !status.is_success() {
        bail!(
            "iLink 请求失败（HTTP {status}）: {}",
            truncate_str(&String::from_utf8_lossy(&bytes), 512)
        );
    }
    if bytes.is_empty() {
        bail!("iLink 响应为空（HTTP {status}）");
    }
    serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "解析 iLink 响应 JSON 失败（HTTP {status}）: {}",
            truncate_str(&String::from_utf8_lossy(&bytes), 512)
        )
    })
}

fn check_ret(body: &Value, api: &str) -> Result<()> {
    for field in ["ret", "errcode"] {
        if let Some(code) = body.get(field).and_then(Value::as_i64)
            && code != 0
        {
            let message = body
                .get("err_msg")
                .or_else(|| body.get("errmsg"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            bail!("iLink {api} 失败: {field}={code}, errmsg={message}");
        }
    }
    Ok(())
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

fn truncate_json(value: &Value) -> String {
    truncate_str(&value.to_string(), 256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_render_content_uses_server_link() {
        let qr = QrCodeResponse {
            qrcode: "qr-id".into(),
            qrcode_img_content: Some("https://example.com/qr".into()),
            qrcode_url: None,
        };
        assert_eq!(qr.render_content(), "https://example.com/qr");
    }

    #[test]
    fn qr_status_matches_current_protocol_values() {
        for (status, expected) in [
            ("wait", QrStatus::Waiting),
            ("scaned", QrStatus::Scanned),
            ("confirmed", QrStatus::Confirmed),
            ("scaned_but_redirect", QrStatus::Redirect),
            ("need_verifycode", QrStatus::NeedVerifyCode),
            ("binded_redirect", QrStatus::AlreadyBound),
        ] {
            let response = QrStatusResponse {
                status: status.into(),
                bot_token: None,
                baseurl: None,
                redirect_host: None,
            };
            assert_eq!(response.status(), expected);
        }
    }

    #[test]
    fn check_ret_rejects_ret_and_errcode() {
        assert!(check_ret(&serde_json::json!({ "ret": 0 }), "test").is_ok());
        assert!(check_ret(&serde_json::json!({ "ret": 1 }), "test").is_err());
        assert!(check_ret(&serde_json::json!({ "errcode": -14 }), "test").is_err());
    }

    #[test]
    fn message_parses_current_image_shape() {
        let response: UpdatesResponse = serde_json::from_value(serde_json::json!({
            "get_updates_buf": "cursor-123",
            "msgs": [{
                "seq": 7,
                "message_id": 42,
                "from_user_id": "u@im.wechat",
                "message_type": 1,
                "context_token": "ctx",
                "item_list": [
                    { "type": 1, "text_item": { "text": "hello" } },
                    { "type": 2, "image_item": { "media": {
                        "encrypt_query_param": "a&b=c",
                        "aes_key": "AA=="
                    } } }
                ]
            }]
        }))
        .unwrap();
        let message = &response.msgs[0];
        assert_eq!(message.text_content(), "hello");
        assert_eq!(message.stable_message_id().as_deref(), Some("42"));
        let image = message.images().next().unwrap();
        let url = build_media_download_url(image).unwrap();
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "encrypted_query_param")
                .map(|(_, value)| value.into_owned())
                .as_deref(),
            Some("a&b=c")
        );
    }

    #[test]
    fn base_url_rejects_non_https_values() {
        assert!(normalize_base_url(ILINK_BASE_URL).is_ok());
        assert!(normalize_base_url("http://127.0.0.1").is_err());
        assert!(normalize_base_url("not-a-url").is_err());
    }
}
