//! bot 制品下载器——拉取 bots-index.json、下载平台制品、SHA256 校验。
//!
//! 端点复用 `tiangong-entry::update` 的 GitHub + 阿里云 OSS 双端点 fallback。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::BotId;
use crate::manifest::{BOTS_INDEX_ENDPOINTS, BotManifest, BotsIndex};
use crate::paths::{self, bot_artifact_path};

/// 下载进度回调：`(已下载字节数, 总字节数)`。
pub type ProgressFn = std::sync::Arc<dyn Fn(u64, u64) + Send + Sync>;

/// bot 制品下载器。
pub struct Downloader {
    http: reqwest::Client,
}

impl Downloader {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("tiangong-bot-downloader")
            .build()
            .context("构建 HTTP 客户端失败")?;
        Ok(Self { http })
    }

    /// 拉取 bots-index.json（双端点 fallback）。
    pub async fn fetch_index(&self) -> Result<BotsIndex> {
        let mut last_err: Option<anyhow::Error> = None;
        for endpoint in BOTS_INDEX_ENDPOINTS {
            match self.fetch_index_from(endpoint).await {
                Ok(index) => return Ok(index),
                Err(err) => {
                    tracing::warn!("拉取 bots-index 失败（{endpoint}）：{err}");
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("无可用 bots-index 端点")))
    }

    async fn fetch_index_from(&self, endpoint: &str) -> Result<BotsIndex> {
        let resp = self
            .http
            .get(endpoint)
            .send()
            .await
            .with_context(|| format!("请求 bots-index 失败：{endpoint}"))?;
        if !resp.status().is_success() {
            bail!("bots-index 响应非 2xx：{} {endpoint}", resp.status());
        }
        let index: BotsIndex = resp
            .json()
            .await
            .with_context(|| format!("解析 bots-index 失败：{endpoint}"))?;
        Ok(index)
    }

    /// 按 manifest 下载当前平台的制品，校验 SHA256 后落到 `bot_artifact_path(id)`。
    ///
    /// `dest_id` 是 bot 实例 id（用于确定落盘目录）。返回制品路径。
    pub async fn install_artifact(
        &self,
        manifest: &BotManifest,
        dest_id: &BotId,
        progress: Option<ProgressFn>,
    ) -> Result<PathBuf> {
        let artifact = manifest.current_artifact().ok_or_else(|| {
            anyhow!(
                "制品 {} 无当前平台 {} 的构建",
                manifest.id,
                crate::manifest::current_platform_key()
            )
        })?;
        paths::reject_symlink(&paths::bot_runtime_dir(dest_id), "Bot 实例目录")?;
        std::fs::create_dir_all(paths::bot_runtime_dir(dest_id))
            .with_context(|| format!("创建 Bot 实例目录失败：{dest_id}"))?;
        paths::ensure_executable_paths_safe(dest_id)?;
        let dest = bot_artifact_path(dest_id);
        self.download_and_verify(&artifact.url, &artifact.checksum, &dest, progress)
            .await?;
        // 赋予可执行权限（Unix）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&dest) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&dest, perms);
            }
        }
        Ok(dest)
    }

    /// 下载单个文件到 `dest`，流式写入并校验 checksum（格式 `sha256:<hex>`）。
    pub async fn download_and_verify(
        &self,
        url: &str,
        checksum: &str,
        dest: &Path,
        progress: Option<ProgressFn>,
    ) -> Result<()> {
        let expected = parse_checksum(checksum)?;

        if let Some(parent) = dest.parent() {
            paths::reject_symlink(parent, "Bot 实例目录")?;
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建制品目录失败：{}", parent.display()))?;
            paths::reject_symlink(parent, "Bot 实例目录")?;
        }
        paths::reject_symlink(dest, "Bot 制品")?;
        let tmp = dest.with_extension(format!("downloading-{}", scru128::new()));
        paths::reject_symlink(&tmp, "Bot 下载临时文件")?;

        let resp = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("请求制品失败：{url}"))?;
        if !resp.status().is_success() {
            bail!("制品下载响应非 2xx：{} {url}", resp.status());
        }
        let total = resp.content_length();

        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut hasher = Sha256::new();
        let mut downloaded: u64 = 0;
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .with_context(|| format!("创建临时文件失败：{}", tmp.display()))?;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("读取制品分片失败")?;
                hasher.update(&chunk);
                file.write_all(&chunk)
                    .with_context(|| format!("写入临时文件失败：{}", tmp.display()))?;
                downloaded += chunk.len() as u64;
                if let Some(ref cb) = progress {
                    cb(downloaded, total.unwrap_or(downloaded));
                }
            }
            file.sync_data().ok();
        }

        let actual = hasher.finalize();
        if actual.as_slice() != expected {
            let _ = std::fs::remove_file(&tmp);
            bail!(
                "制品 SHA256 校验失败：期望 {}，实际 {}",
                hex::encode(expected),
                hex::encode(actual)
            );
        }

        if let Some(parent) = dest.parent() {
            paths::reject_symlink(parent, "Bot 实例目录")?;
        }
        paths::reject_symlink(dest, "Bot 制品")?;
        paths::reject_symlink(&tmp, "Bot 下载临时文件")?;
        std::fs::rename(&tmp, dest)
            .with_context(|| format!("重命名制品失败：{} -> {}", tmp.display(), dest.display()))?;
        tracing::info!("制品下载完成：{}", dest.display());
        Ok(())
    }
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new().expect("构建 downloader 失败")
    }
}

/// 解析 `sha256:<hex>` 格式的 checksum，返回原始字节摘要。
fn parse_checksum(checksum: &str) -> Result<Vec<u8>> {
    let hex_str = checksum
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("checksum 格式错误，需以 sha256: 开头：{checksum}"))?;
    let bytes = hex::decode(hex_str).context("checksum hex 解码失败")?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn download_and_verify_ok() {
        let server = MockServer::start().await;
        let payload = b"hello bot artifact";
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let digest = hasher.finalize();
        let checksum = format!("sha256:{}", hex::encode(digest));

        Mock::given(method("GET"))
            .and(path("/artifact"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.to_vec()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bot");
        let dl = Downloader::new().unwrap();
        dl.download_and_verify(
            &format!("{}/artifact", server.uri()),
            &checksum,
            &dest,
            None,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
    }

    #[tokio::test]
    async fn download_rejects_bad_checksum() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/artifact"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bot");
        let dl = Downloader::new().unwrap();
        let err = dl
            .download_and_verify(
                &format!("{}/artifact", server.uri()),
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                &dest,
                None,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("SHA256 校验失败"));
        assert!(!dest.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn download_rejects_symlink_destination() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"old").unwrap();
        let dest = dir.path().join("bot");
        symlink(&target, &dest).unwrap();

        let error = Downloader::new()
            .unwrap()
            .download_and_verify(
                "http://127.0.0.1:1/not-used",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                &dest,
                None,
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("符号链接"));
        assert_eq!(std::fs::read(target).unwrap(), b"old");
    }
}
