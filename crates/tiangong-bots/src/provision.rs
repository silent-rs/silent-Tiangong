//! bot 扫码配置子进程协议。
//!
//! 主程序不实现任何平台授权逻辑，只调用 bot 制品提供的
//! `--provision-begin` / `--provision-poll` 命令并转发 JSON 结果。

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const BEGIN_COMMAND: &str = "--provision-begin";
const POLL_COMMAND: &str = "--provision-poll";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(40);

/// 扫码会话（由 bot 的 begin 命令产出）。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
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

/// 轮询扫码授权的状态。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProvisionStatus {
    /// 等待用户扫码授权。
    Pending {
        /// bot 要求调整后的下次轮询间隔；未提供时沿用扫码会话的间隔。
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_after: Option<u64>,
    },
    /// 授权成功；扫码所得配置已由 bot 自行处理。
    Success,
    /// 扫码会话已过期。
    Expired,
    /// 授权失败。
    Error { message: String },
}

/// 调用 bot 创建扫码会话。
pub async fn begin(artifact_path: &Path) -> Result<QrSession> {
    invoke(artifact_path, BEGIN_COMMAND, None).await
}

/// 调用 bot 轮询扫码状态。
pub async fn poll(artifact_path: &Path, session: &QrSession) -> Result<ProvisionStatus> {
    let input = serde_json::to_vec(session).context("序列化扫码会话失败")?;
    invoke(artifact_path, POLL_COMMAND, Some(input)).await
}

async fn invoke<T>(artifact_path: &Path, command: &str, input: Option<Vec<u8>>) -> Result<T>
where
    T: DeserializeOwned,
{
    if !artifact_path.exists() {
        bail!("bot 制品不存在：{}", artifact_path.display());
    }

    let output = tokio::time::timeout(COMMAND_TIMEOUT, execute(artifact_path, command, input))
        .await
        .with_context(|| format!("bot 扫码命令执行超时：{command}"))??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "bot 扫码命令执行失败：{} stderr={}",
            output.status,
            stderr.chars().take(1024).collect::<String>()
        );
    }

    serde_json::from_slice(&output.stdout).context("bot 扫码命令返回了无效数据")
}

async fn execute(artifact_path: &Path, command: &str, input: Option<Vec<u8>>) -> Result<Output> {
    let mut process = Command::new(artifact_path);
    process
        .arg(command)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    tiangong_types::process::configure_tokio_no_window(&mut process);

    let mut child = process
        .spawn()
        .with_context(|| format!("启动 bot 扫码命令失败：{}", artifact_path.display()))?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().context("打开 bot 扫码命令输入失败")?;
        stdin
            .write_all(&input)
            .await
            .context("写入 bot 扫码会话失败")?;
        stdin
            .shutdown()
            .await
            .context("关闭 bot 扫码命令输入失败")?;
    }

    child
        .wait_with_output()
        .await
        .context("等待 bot 扫码命令失败")
}
