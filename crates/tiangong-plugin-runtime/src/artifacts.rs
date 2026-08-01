//! 基于 OSS 静态目录的插件制品发现、下载与校验。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{MANIFEST_FILE, PluginManifest};

pub const PLUGIN_CATALOG_ENDPOINT: &str =
    "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com/plugins-index/catalog.json";
const CATALOG_VERSION: u32 = 1;
const TRANSACTIONS_DIR: &str = ".transactions";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCatalog {
    pub version: u32,
    pub plugins: Vec<PluginRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRelease {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub manifest: RemoteArtifact,
    pub wasm: RemoteArtifact,
    #[serde(default)]
    pub sidecars: BTreeMap<String, RemoteArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteArtifact {
    pub url: String,
    pub checksum: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AvailablePlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub supported: bool,
    pub installed_version: Option<String>,
    pub update_available: bool,
}

pub struct StagedPlugin {
    path: PathBuf,
}

impl StagedPlugin {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedPlugin {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), %error, "清理插件临时目录失败");
        }
    }
}

/// 将用户选择的本地完整插件目录复制到受管事务目录。
pub fn stage_local_plugin(storage_root: &Path, source: &Path) -> Result<StagedPlugin> {
    ensure_source_directory(source)?;
    let source_manifest = source.join(MANIFEST_FILE);
    ensure_regular_file(&source_manifest, "插件清单")?;
    let manifest = PluginManifest::load(&source_manifest)?;
    Version::parse(&manifest.version)
        .with_context(|| format!("本地插件 {} 版本不是有效语义版本", manifest.id))?;

    let staged = create_staged_plugin(storage_root)?;
    copy_regular_file(
        &source_manifest,
        &staged.path.join(MANIFEST_FILE),
        "插件清单",
    )?;
    copy_local_artifact(source, manifest.wasm_binary(), &staged.path, "WASM 制品")?;

    if let Some(sidecar) = &manifest.sidecar {
        let binary = with_executable_suffix(&sidecar.binary)?;
        let destination = copy_local_artifact(source, &binary, &staged.path, "sidecar 制品")?;
        set_executable(&destination)?;
    }

    for directory in ["runtime", "logs", "data"] {
        std::fs::create_dir_all(staged.path.join(directory))?;
    }
    Ok(staged)
}

pub struct PluginRepository {
    http: reqwest::Client,
}

impl PluginRepository {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("tiangong-plugin-downloader")
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()
            .context("构建插件下载客户端失败")?;
        Ok(Self { http })
    }

    pub async fn list_available(&self, storage_root: &Path) -> Result<Vec<AvailablePlugin>> {
        let catalog = self.fetch_catalog().await?;
        let installed = installed_versions(storage_root);
        let platform = current_platform_key();
        Ok(catalog
            .plugins
            .into_iter()
            .map(|plugin| {
                let installed_version = installed.get(&plugin.id).cloned();
                let update_available = installed_version
                    .as_deref()
                    .is_some_and(|local| version_is_newer(local, &plugin.version));
                AvailablePlugin {
                    supported: plugin.sidecars.is_empty()
                        || plugin.sidecars.contains_key(&platform),
                    id: plugin.id,
                    name: plugin.name,
                    version: plugin.version,
                    description: plugin.description,
                    installed_version,
                    update_available,
                }
            })
            .collect())
    }

    pub async fn download(&self, storage_root: &Path, plugin_id: &str) -> Result<StagedPlugin> {
        let catalog = self.fetch_catalog().await?;
        let release = catalog
            .plugins
            .into_iter()
            .find(|plugin| plugin.id == plugin_id)
            .ok_or_else(|| anyhow!("OSS 插件目录中不存在: {plugin_id}"))?;
        self.download_release(storage_root, &release).await
    }

    async fn fetch_catalog(&self) -> Result<PluginCatalog> {
        let endpoint = catalog_endpoint();
        validate_download_url(&endpoint, "OSS 插件目录")?;
        let response = self
            .http
            .get(&endpoint)
            .send()
            .await
            .with_context(|| format!("请求 OSS 插件目录失败: {endpoint}"))?;
        if !response.status().is_success() {
            bail!("OSS 插件目录响应异常: {} {endpoint}", response.status());
        }
        let mut catalog: PluginCatalog = response
            .json()
            .await
            .with_context(|| format!("解析 OSS 插件目录失败: {endpoint}"))?;
        validate_catalog(&catalog)?;
        catalog
            .plugins
            .sort_by(|left, right| left.id.cmp(&right.id));
        Ok(catalog)
    }

    async fn download_release(
        &self,
        storage_root: &Path,
        release: &PluginRelease,
    ) -> Result<StagedPlugin> {
        let staged = create_staged_plugin(storage_root)?;

        let manifest_path = staged.path.join(MANIFEST_FILE);
        validate_artifact_file_name(&release.manifest.url, MANIFEST_FILE)?;
        self.download_file(&release.manifest, &manifest_path)
            .await?;
        let manifest = PluginManifest::load(&manifest_path)?;
        if manifest.id != release.id {
            bail!(
                "插件目录 ID 与清单不一致: catalog={}, manifest={}",
                release.id,
                manifest.id
            );
        }
        if manifest.version != release.version {
            bail!(
                "插件目录版本与清单不一致: catalog={}, manifest={}",
                release.version,
                manifest.version
            );
        }

        let wasm_path = staged.path.join(manifest.wasm_binary());
        let wasm_name = manifest
            .wasm_binary()
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("WASM 制品文件名无效"))?;
        validate_artifact_file_name(&release.wasm.url, wasm_name)?;
        self.download_file(&release.wasm, &wasm_path).await?;

        match &manifest.sidecar {
            Some(sidecar) => {
                let platform = current_platform_key();
                let artifact = release.sidecars.get(&platform).ok_or_else(|| {
                    anyhow!("插件 {} 没有当前平台 {platform} 的 sidecar", release.id)
                })?;
                let sidecar_path = staged.path.join(with_executable_suffix(&sidecar.binary)?);
                let sidecar_name = sidecar_path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| anyhow!("sidecar 文件名无效"))?;
                validate_artifact_file_name(&artifact.url, sidecar_name)?;
                self.download_file(artifact, &sidecar_path).await?;
                set_executable(&sidecar_path)?;
            }
            None if !release.sidecars.is_empty() => {
                bail!(
                    "插件 {} 未声明 sidecar，但 OSS 目录包含 sidecar",
                    release.id
                );
            }
            None => {}
        }

        for directory in ["runtime", "logs", "data"] {
            std::fs::create_dir_all(staged.path.join(directory))?;
        }
        Ok(staged)
    }

    async fn download_file(&self, artifact: &RemoteArtifact, destination: &Path) -> Result<()> {
        let expected = parse_checksum(&artifact.checksum)?;
        validate_download_url(&artifact.url, "插件制品")?;
        if let Some(parent) = destination.parent() {
            ensure_not_symlink(parent)?;
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建插件制品目录失败: {}", parent.display()))?;
        }
        ensure_not_symlink(destination)?;

        let response = self
            .http
            .get(&artifact.url)
            .send()
            .await
            .with_context(|| format!("下载插件制品失败: {}", artifact.url))?;
        if !response.status().is_success() {
            bail!(
                "插件制品下载响应异常: {} {}",
                response.status(),
                artifact.url
            );
        }

        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .with_context(|| format!("创建插件制品失败: {}", destination.display()))?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("读取插件制品失败: {}", artifact.url))?;
            hasher.update(&chunk);
            file.write_all(&chunk)
                .with_context(|| format!("写入插件制品失败: {}", destination.display()))?;
        }
        file.sync_all()
            .with_context(|| format!("同步插件制品失败: {}", destination.display()))?;
        let actual = hasher.finalize();
        if actual.as_slice() != expected {
            let _ = std::fs::remove_file(destination);
            bail!(
                "插件制品 SHA-256 校验失败: expected={}, actual={}",
                hex::encode(expected),
                hex::encode(actual)
            );
        }
        Ok(())
    }
}

fn create_staged_plugin(storage_root: &Path) -> Result<StagedPlugin> {
    let root = storage_root.join("plugins").join(TRANSACTIONS_DIR);
    ensure_not_symlink(&root)?;
    std::fs::create_dir_all(&root)
        .with_context(|| format!("创建插件事务目录失败: {}", root.display()))?;
    let path = root.join(scru128::new().to_string());
    std::fs::create_dir(&path)
        .with_context(|| format!("创建插件临时目录失败: {}", path.display()))?;
    Ok(StagedPlugin { path })
}

fn copy_local_artifact(
    source_root: &Path,
    relative_path: &Path,
    destination_root: &Path,
    label: &str,
) -> Result<PathBuf> {
    let source = resolve_local_artifact(source_root, relative_path, label)?;
    let destination = destination_root.join(relative_path);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建{label}目录失败: {}", parent.display()))?;
    }
    copy_regular_file(&source, &destination, label)?;
    Ok(destination)
}

fn resolve_local_artifact(
    source_root: &Path,
    relative_path: &Path,
    label: &str,
) -> Result<PathBuf> {
    let mut source = source_root.to_path_buf();
    for component in relative_path.components() {
        source.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&source)
            .with_context(|| format!("读取{label}失败: {}", source.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("{label}路径不能包含符号链接: {}", source.display());
        }
    }
    ensure_regular_file(&source, label)?;
    Ok(source)
}

fn copy_regular_file(source: &Path, destination: &Path, label: &str) -> Result<()> {
    std::fs::copy(source, destination).with_context(|| {
        format!(
            "复制{label}失败: {} -> {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn ensure_source_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("读取本地插件目录失败: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("本地插件路径必须是实际目录: {}", path.display());
    }
    Ok(())
}

fn ensure_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("读取{label}失败: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label}必须是实际文件: {}", path.display());
    }
    Ok(())
}

fn with_executable_suffix(path: &Path) -> Result<PathBuf> {
    let mut path = path.to_path_buf();
    if !std::env::consts::EXE_SUFFIX.is_empty() {
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("sidecar 文件名无效"))?;
        if !file_name.ends_with(std::env::consts::EXE_SUFFIX) {
            path.set_file_name(format!("{file_name}{}", std::env::consts::EXE_SUFFIX));
        }
    }
    Ok(path)
}

fn validate_catalog(catalog: &PluginCatalog) -> Result<()> {
    if catalog.version != CATALOG_VERSION {
        bail!(
            "不支持的 OSS 插件目录版本: expected={CATALOG_VERSION}, actual={}",
            catalog.version
        );
    }
    let mut ids = HashSet::new();
    for plugin in &catalog.plugins {
        if plugin.id.is_empty() || !ids.insert(plugin.id.clone()) {
            bail!("OSS 插件目录包含空 ID 或重复 ID: {}", plugin.id);
        }
        Version::parse(&plugin.version)
            .with_context(|| format!("插件 {} 版本不是有效语义版本", plugin.id))?;
        validate_download_url(&plugin.manifest.url, "插件清单")?;
        validate_download_url(&plugin.wasm.url, "WASM 制品")?;
        parse_checksum(&plugin.manifest.checksum)?;
        parse_checksum(&plugin.wasm.checksum)?;
        for artifact in plugin.sidecars.values() {
            validate_download_url(&artifact.url, "sidecar 制品")?;
            parse_checksum(&artifact.checksum)?;
        }
    }
    Ok(())
}

fn installed_versions(storage_root: &Path) -> HashMap<String, String> {
    let plugins_dir = storage_root.join("plugins");
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return HashMap::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| PluginManifest::load(&entry.path().join(MANIFEST_FILE)).ok())
        .map(|manifest| (manifest.id, manifest.version))
        .collect()
}

fn version_is_newer(installed: &str, available: &str) -> bool {
    match (Version::parse(installed), Version::parse(available)) {
        (Ok(installed), Ok(available)) => available > installed,
        _ => available != installed,
    }
}

fn parse_checksum(checksum: &str) -> Result<Vec<u8>> {
    let value = checksum
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("制品 checksum 必须使用 sha256:<hex> 格式"))?;
    let bytes = hex::decode(value).context("制品 checksum 不是有效十六进制")?;
    if bytes.len() != 32 {
        bail!("制品 SHA-256 长度无效");
    }
    Ok(bytes)
}

fn validate_artifact_file_name(url: &str, expected: &str) -> Result<()> {
    let url = reqwest::Url::parse(url).with_context(|| format!("插件制品 URL 无效: {url}"))?;
    let actual = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .ok_or_else(|| anyhow!("插件制品 URL 缺少文件名: {url}"))?;
    if actual != expected {
        bail!("插件制品名称不一致: expected={expected}, actual={actual}");
    }
    Ok(())
}

fn validate_download_url(url: &str, label: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("{label} URL 无效: {url}"))?;
    let local_http = parsed.scheme() == "http"
        && parsed
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"));
    if parsed.scheme() != "https" && !local_http {
        bail!("{label} 必须使用 HTTPS: {url}");
    }
    Ok(())
}

fn catalog_endpoint() -> String {
    std::env::var("TIANGONG_PLUGIN_CATALOG_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| PLUGIN_CATALOG_ENDPOINT.to_string())
}

pub fn current_platform_key() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "unknown"
    };
    format!("{os}-{arch}")
}

fn ensure_not_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("插件路径不能是符号链接: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("检查插件路径失败: {}", path.display())),
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}
