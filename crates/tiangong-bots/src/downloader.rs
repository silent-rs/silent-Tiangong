//! bot 制品下载器——合并独立 bot 索引、下载平台制品、SHA256 校验。

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::manifest::{BOTS_INDEX_CATALOG_ENDPOINT, BotManifest, BotsIndex, BotsIndexCatalog};
use crate::paths;

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

    /// 并行拉取各 bot 的独立索引，返回所有成功结果的合集。
    pub async fn fetch_index(&self) -> Result<BotsIndex> {
        let catalog = self.fetch_index_catalog().await?;
        let requests = catalog
            .indexes
            .iter()
            .map(|endpoint| async move { (endpoint, self.fetch_index_from(endpoint).await) });
        let results = futures_util::future::join_all(requests).await;

        let mut version: Option<u32> = None;
        let mut bots = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut last_err: Option<anyhow::Error> = None;
        for (endpoint, result) in results {
            match result {
                Ok(index) => {
                    if let Some(current_version) = version {
                        if current_version != index.version {
                            tracing::warn!(
                                "忽略 bot 索引格式版本差异（{endpoint}）：期望 {}，实际 {}",
                                current_version,
                                index.version
                            );
                        }
                    } else {
                        version = Some(index.version);
                    }
                    for bot in index.bots {
                        if seen_ids.insert(bot.id.clone()) {
                            bots.push(bot);
                        } else {
                            tracing::warn!("忽略 bot 索引中的重复 id（{endpoint}）：{}", bot.id);
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("拉取 bots-index 失败（{endpoint}）：{err}");
                    last_err = Some(err);
                }
            }
        }
        if let Some(version) = version {
            bots.sort_by(|left, right| left.id.cmp(&right.id));
            return Ok(BotsIndex { version, bots });
        }

        Err(last_err.unwrap_or_else(|| anyhow!("无可用 bot 索引端点")))
    }

    async fn fetch_index_catalog(&self) -> Result<BotsIndexCatalog> {
        let endpoint = BOTS_INDEX_CATALOG_ENDPOINT;
        let resp = self
            .http
            .get(endpoint)
            .send()
            .await
            .with_context(|| format!("请求 bot 索引目录失败：{endpoint}"))?;
        if !resp.status().is_success() {
            bail!("bot 索引目录响应非 2xx：{} {endpoint}", resp.status());
        }
        let catalog: BotsIndexCatalog = resp
            .json()
            .await
            .with_context(|| format!("解析 bot 索引目录失败：{endpoint}"))?;
        if catalog.version != 1 {
            bail!("不支持的 bot 索引目录版本：{}", catalog.version);
        }
        if catalog.indexes.is_empty() {
            bail!("bot 索引目录不含任何索引：{endpoint}");
        }
        Ok(catalog)
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
        if index.bots.is_empty() {
            bail!("bots-index 不含任何 bot：{endpoint}");
        }
        Ok(index)
    }

    /// 按 manifest 下载当前平台的制品到指定临时路径并校验 SHA256。
    pub async fn download_artifact_to(
        &self,
        manifest: &BotManifest,
        dest: &Path,
        progress: Option<ProgressFn>,
    ) -> Result<()> {
        let artifact = manifest.current_artifact().ok_or_else(|| {
            anyhow!(
                "制品 {} 无当前平台 {} 的构建",
                manifest.id,
                crate::manifest::current_platform_key()
            )
        })?;
        self.download_and_verify(&artifact.url, &artifact.checksum, dest, progress)
            .await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(dest)
                .with_context(|| format!("读取制品权限失败：{}", dest.display()))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(dest, perms)
                .with_context(|| format!("设置制品执行权限失败：{}", dest.display()))?;
        }
        Ok(())
    }

    /// 下载单个文件到不存在的 `dest`，流式写入并校验 checksum（格式 `sha256:<hex>`）。
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
        paths::reject_symlink(dest, "Bot 下载临时文件")?;

        let mut created = false;
        let result: Result<()> = async {
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
                    .open(dest)
                    .with_context(|| format!("创建临时文件失败：{}", dest.display()))?;
                created = true;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.context("读取制品分片失败")?;
                    hasher.update(&chunk);
                    file.write_all(&chunk)
                        .with_context(|| format!("写入临时文件失败：{}", dest.display()))?;
                    downloaded += chunk.len() as u64;
                    if let Some(ref cb) = progress {
                        cb(downloaded, total.unwrap_or(downloaded));
                    }
                }
                file.sync_all()
                    .with_context(|| format!("同步临时文件失败：{}", dest.display()))?;
            }

            let actual = hasher.finalize();
            if actual.as_slice() != expected {
                bail!(
                    "制品 SHA256 校验失败：期望 {}，实际 {}",
                    hex::encode(expected),
                    hex::encode(actual)
                );
            }
            Ok(())
        }
        .await;

        if result.is_err() && created {
            let _ = std::fs::remove_file(dest);
        }
        result?;
        tracing::info!("制品下载并校验完成：{}", dest.display());
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

    #[tokio::test]
    async fn download_never_overwrites_existing_destination() {
        let server = MockServer::start().await;
        let payload = b"new artifact";
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let checksum = format!("sha256:{}", hex::encode(hasher.finalize()));

        Mock::given(method("GET"))
            .and(path("/artifact"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.to_vec()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bot");
        std::fs::write(&dest, b"old artifact").unwrap();
        let err = Downloader::new()
            .unwrap()
            .download_and_verify(
                &format!("{}/artifact", server.uri()),
                &checksum,
                &dest,
                None,
            )
            .await
            .unwrap_err();

        assert!(format!("{err}").contains("创建临时文件失败"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"old artifact");
    }
}
