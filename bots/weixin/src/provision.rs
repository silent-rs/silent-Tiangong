//! 微信 iLink 扫码登录协议。
//!
//! 复用主程序的通用扫码配置协议（`--provision-begin` / `--provision-poll`），
//! 与飞书 bot 同构：主程序只转发 JSON，不解释 `state` 字段。
//!
//! 流程：
//! 1. `begin()`：请求 iLink 获取二维码 URL + 会话标识
//! 2. `poll()`：长轮询扫码状态，成功时保存 `bot_token` 到 `credentials.json`

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::Local;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ilink;

const CREDENTIALS_FILE: &str = "credentials.json";

/// 扫码会话（与主程序 `QrSession` 契约一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrSession {
    /// 扫码 URL（前端渲染为二维码）。
    pub qr_url: String,
    /// 过期时间戳（Unix 秒）。
    pub expires_at: i64,
    /// 轮询间隔（秒）。
    pub interval: u64,
    /// bot 自己维护的会话状态，主程序不读取或解释。
    pub state: Value,
}

/// 轮询扫码授权的状态（与主程序 `ProvisionStatus` 契约一致）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProvisionStatus {
    Pending {
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after: Option<u64>,
    },
    Success,
    Expired,
    Error {
        message: String,
    },
}

/// 扫码会话内部状态（仅 bot 自己使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WeixinSessionState {
    /// iLink 返回的 qrcode 标识（用于轮询扫码状态）。
    qrcode: String,
}

/// 扫码所得凭证。
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub bot_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

// ── 凭证加载 ──────────────────────────────────────────────────

/// 加载凭证：优先读扫码保存的 `credentials.json`，再回退环境变量和实例配置。
pub fn load_credentials() -> Result<Credentials> {
    if let Some(credentials) = load_provisioned_credentials()? {
        return Ok(credentials);
    }

    if let Ok(bot_token) = std::env::var("TIANGONG_BOT_WEIXIN_TOKEN") {
        return validate_credentials(Credentials {
            bot_token,
            base_url: None,
        });
    }

    if let Some(credentials) = load_runtime_bot_credentials()? {
        return Ok(credentials);
    }

    bail!("尚未扫码配置，且当前 Bot 配置缺少 bot_token")
}

// ── 扫码流程 ──────────────────────────────────────────────────

/// 启动扫码会话：请求 iLink 获取二维码。
pub async fn begin() -> Result<QrSession> {
    let client = Client::builder().build().context("构建 HTTP 客户端失败")?;

    let local_tokens = load_provisioned_credentials()?
        .map(|credentials| vec![credentials.bot_token])
        .unwrap_or_default();
    let qr = ilink::get_bot_qrcode(&client, &local_tokens).await?;
    if qr.qrcode.trim().is_empty() {
        bail!("iLink 未返回二维码标识");
    }

    let expires_at = Local::now()
        .timestamp()
        .saturating_add(ilink::DEFAULT_QR_EXPIRES_IN);

    Ok(QrSession {
        qr_url: qr.render_content(),
        expires_at,
        interval: ilink::DEFAULT_QR_POLL_INTERVAL,
        state: serde_json::to_value(WeixinSessionState { qrcode: qr.qrcode })
            .context("保存微信扫码会话失败")?,
    })
}

/// 从 stdin 读取扫码会话并轮询状态（供 `--provision-poll` 使用）。
pub async fn poll_from_stdin() -> Result<ProvisionStatus> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("读取扫码会话失败")?;
    let session: QrSession = serde_json::from_str(&input).context("解析扫码会话失败")?;
    poll(&session).await
}

/// 轮询扫码状态。
async fn poll(session: &QrSession) -> Result<ProvisionStatus> {
    if Local::now().timestamp() >= session.expires_at {
        return Ok(ProvisionStatus::Expired);
    }

    let state: WeixinSessionState =
        serde_json::from_value(session.state.clone()).context("扫码会话状态无效")?;
    if state.qrcode.trim().is_empty() {
        bail!("扫码会话缺少二维码标识");
    }

    let client = Client::builder().build().context("构建 HTTP 客户端失败")?;
    let mut base_url = ilink::ILINK_BASE_URL.to_string();

    for _ in 0..3 {
        let Some(response) = ilink::get_qrcode_status(&client, &base_url, &state.qrcode).await?
        else {
            return Ok(ProvisionStatus::Pending { retry_after: None });
        };

        match response.status() {
            ilink::QrStatus::Confirmed => {
                let bot_token = response
                    .bot_token
                    .filter(|token| !token.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("iLink 授权成功但未返回 bot_token"))?;
                let response_base_url = response
                    .baseurl
                    .filter(|url| !url.trim().is_empty())
                    .or_else(|| (base_url != ilink::ILINK_BASE_URL).then_some(base_url));
                save_credentials(&Credentials {
                    bot_token,
                    base_url: response_base_url,
                })?;
                tracing::info!("微信扫码授权成功，凭证已保存");
                return Ok(ProvisionStatus::Success);
            }
            ilink::QrStatus::Waiting | ilink::QrStatus::Scanned => {
                return Ok(ProvisionStatus::Pending { retry_after: None });
            }
            ilink::QrStatus::Expired => return Ok(ProvisionStatus::Expired),
            ilink::QrStatus::Canceled => {
                return Ok(ProvisionStatus::Error {
                    message: "已取消微信授权".to_string(),
                });
            }
            ilink::QrStatus::Redirect => {
                let redirect_host = response.redirect_host.as_deref().unwrap_or_default();
                base_url = ilink::redirect_base_url(redirect_host)?;
            }
            ilink::QrStatus::NeedVerifyCode => {
                return Ok(ProvisionStatus::Error {
                    message: "当前微信账号需要输入手机上显示的配对码，请重新扫码或改用 Bot Token 手工配置"
                        .to_string(),
                });
            }
            ilink::QrStatus::VerifyCodeBlocked => {
                return Ok(ProvisionStatus::Error {
                    message: "微信配对码验证次数过多，请稍后重新生成二维码".to_string(),
                });
            }
            ilink::QrStatus::AlreadyBound => {
                return Ok(if load_provisioned_credentials()?.is_some() {
                    ProvisionStatus::Success
                } else {
                    ProvisionStatus::Error {
                        message: "该微信账号已绑定，但本地没有可用凭证，请改用 Bot Token 手工配置"
                            .to_string(),
                    }
                });
            }
            ilink::QrStatus::Unknown => {
                return Ok(ProvisionStatus::Error {
                    message: format!("微信返回了无法识别的扫码状态：{}", response.status),
                });
            }
        }
    }

    Ok(ProvisionStatus::Error {
        message: "微信扫码服务连续重定向次数过多，请重新生成二维码".to_string(),
    })
}

// ── 凭证持久化 ────────────────────────────────────────────────

fn load_provisioned_credentials() -> Result<Option<Credentials>> {
    let path = credentials_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read(&path)
        .with_context(|| format!("读取微信 bot 配置失败：{}", path.display()))?;
    let credentials = serde_json::from_slice(&content)
        .with_context(|| format!("解析微信 bot 配置失败：{}", path.display()))?;
    validate_credentials(credentials).map(Some)
}

/// 普通 MCP 注册不会携带 Bot 进程环境变量，因此读取当前实例在 `bots.json`
/// 中已有的手工配置。这里只读取，不复制凭证。
fn load_runtime_bot_credentials() -> Result<Option<Credentials>> {
    let executable = std::env::current_exe().context("获取微信 bot 路径失败")?;
    let runtime_dir = executable.parent().context("微信 bot 路径缺少父目录")?;
    let bot_id = runtime_dir
        .file_name()
        .and_then(|name| name.to_str())
        .context("微信 bot 实例目录名称无效")?;
    let bots_dir = runtime_dir.parent().context("微信 bot 路径缺少配置目录")?;
    let config_path = bots_dir.join("bots.json");
    if !config_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read(&config_path)
        .with_context(|| format!("读取 Bot 配置失败：{}", config_path.display()))?;
    let root: Value = serde_json::from_slice(&content)
        .with_context(|| format!("解析 Bot 配置失败：{}", config_path.display()))?;
    let Some(bot) = root["bots"]
        .as_array()
        .and_then(|bots| bots.iter().find(|bot| bot["id"].as_str() == Some(bot_id)))
    else {
        return Ok(None);
    };
    let Some(bot_token) = bot["config"]["bot_token"].as_str() else {
        return Ok(None);
    };
    validate_credentials(Credentials {
        bot_token: bot_token.to_string(),
        base_url: None,
    })
    .map(Some)
}

fn save_credentials(credentials: &Credentials) -> Result<()> {
    let credentials = validate_credentials(credentials.clone())?;
    let path = credentials_path()?;
    let content = serde_json::to_vec(&credentials).context("序列化微信 bot 配置失败")?;

    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options
        .open(&path)
        .with_context(|| format!("保存微信 bot 配置失败：{}", path.display()))?;
    file.write_all(&content)
        .with_context(|| format!("写入微信 bot 配置失败：{}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("同步微信 bot 配置失败：{}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("设置微信 bot 配置权限失败：{}", path.display()))?;
    }

    Ok(())
}

fn validate_credentials(credentials: Credentials) -> Result<Credentials> {
    let bot_token = credentials.bot_token.trim().to_string();
    if bot_token.is_empty() {
        bail!("微信 bot 配置缺少 bot_token");
    }
    let base_url = credentials
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ilink::normalize_base_url)
        .transpose()?;
    Ok(Credentials {
        bot_token,
        base_url,
    })
}

fn credentials_path() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("获取微信 bot 路径失败")?;
    let directory = executable.parent().context("微信 bot 路径缺少父目录")?;
    Ok(directory.join(CREDENTIALS_FILE))
}
