//! bot 运行时——按 bot 实例启停、健康检查的运行时表。
//!
//! 组合 [`crate::store`] 的配置与 [`crate::supervisor`] 的进程监督。
//! bot 凭证由 [`BotConfig::config`] 转成环境变量注入子进程（飞书用
//! `TIANGONG_BOT_FEISHU_*` 前缀，见 [`bot_env`])。

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::BotId;
use crate::config::{
    BotConfig, BotDescription, BotMcpConfig, ConfigFieldSchema, PushTargetList, PushTargetView,
};
use crate::downloader::{Downloader, ProgressFn};
use crate::management::BotStore;
use crate::manifest::BotManifest;
use crate::paths;

/// bot 运行时健康状态。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BotHealth {
    /// 进程运行中。
    Running,
    /// 已停止。
    Stopped,
    /// 缺少制品（需先安装）。
    MissingArtifact,
    /// 启动/运行出错。
    Error { message: String },
}

/// 本地已安装的制品（扫描 `~/.tiangong/bots/*/` 发现）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocalArtifact {
    /// 目录名（即 bot 实例 id）。
    pub id: String,
    /// 制品展示名称。
    pub name: String,
    /// 制品 id（来自 version.json 或 schema.json 的推断）。
    pub artifact_id: String,
    /// 已安装版本（来自 version.json，未知则为空）。
    pub version: String,
    /// config schema（来自 schema.json 缓存）。
    pub config_schema: Vec<ConfigFieldSchema>,
    /// 是否声明了主动推送 MCP 能力。
    pub supports_mcp: bool,
}

/// 被运行时管理的 bot 条目。

#[derive(Clone)]
struct ArtifactFiles {
    artifact: PathBuf,
    schema: PathBuf,
    description: PathBuf,
    version: PathBuf,
}

impl ArtifactFiles {
    fn for_bot(id: &BotId) -> Self {
        Self {
            artifact: paths::bot_artifact_path(id),
            schema: paths::bot_schema_path(id),
            description: paths::bot_description_path(id),
            version: paths::bot_version_path(id),
        }
    }

    fn transaction_files(&self, purpose: &str, transaction_id: &str) -> Self {
        Self {
            artifact: transaction_path(&self.artifact, purpose, transaction_id),
            schema: transaction_path(&self.schema, purpose, transaction_id),
            description: transaction_path(&self.description, purpose, transaction_id),
            version: transaction_path(&self.version, purpose, transaction_id),
        }
    }

    fn all(&self) -> [&Path; 4] {
        [
            &self.artifact,
            &self.schema,
            &self.description,
            &self.version,
        ]
    }
}

struct StagedFiles {
    files: ArtifactFiles,
}

impl Drop for StagedFiles {
    fn drop(&mut self) {
        for path in self.files.all() {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct FileReplacement {
    target: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
}

/// bot 运行时表。
pub struct BotRuntime {
    store: Arc<BotStore>,
    downloader: Downloader,
    /// 制品 manifest 缓存（artifact_id → manifest），由安装时填入。
    manifests: Mutex<HashMap<String, BotManifest>>,
}

impl BotRuntime {
    pub fn new(store: Arc<BotStore>) -> Result<Self> {
        Ok(Self {
            store,
            downloader: Downloader::new()?,
            manifests: Mutex::new(HashMap::new()),
        })
    }

    /// 安装（下载 + 校验）某制品到 bot 实例目录。
    ///
    /// 安装成功后自动调用 `bot --describe` 获取并缓存 config schema，
    /// 并写入版本记录（version.json）。
    pub async fn install(
        &self,
        manifest: BotManifest,
        dest_id: &BotId,
        progress: Option<ProgressFn>,
    ) -> Result<PathBuf> {
        let artifact_id = manifest.id.clone();
        let runtime_dir = paths::bot_runtime_dir(dest_id);
        paths::reject_symlink(&runtime_dir, "Bot 实例目录")?;
        std::fs::create_dir_all(&runtime_dir)
            .with_context(|| format!("创建 Bot 实例目录失败：{dest_id}"))?;
        paths::ensure_executable_paths_safe(dest_id)?;
        let files = ArtifactFiles::for_bot(dest_id);
        self.install_to_files(&manifest, &files, progress).await?;
        self.manifests.lock().await.insert(artifact_id, manifest);
        Ok(files.artifact)
    }

    async fn install_to_files(
        &self,
        manifest: &BotManifest,
        files: &ArtifactFiles,
        progress: Option<ProgressFn>,
    ) -> Result<()> {
        let transaction_id = scru128::new().to_string();
        let staged = files.transaction_files("download", &transaction_id);
        let backups = files.transaction_files("backup", &transaction_id);
        for path in files.all() {
            paths::reject_symlink(path, "Bot 安装文件")?;
        }
        for path in staged.all() {
            paths::reject_symlink(path, "Bot 升级临时文件")?;
        }
        for path in backups.all() {
            paths::reject_symlink(path, "Bot 升级备份文件")?;
        }
        let _staged_cleanup = StagedFiles {
            files: staged.clone(),
        };

        self.downloader
            .download_artifact_to(manifest, &staged.artifact, progress)
            .await?;
        let description = describe_artifact(&staged.artifact).await?;
        validate_description(&description, manifest)?;

        let schema_content = serde_json::to_string_pretty(&description.config_schema)
            .context("序列化 bot schema 失败")?;
        write_new_file(&staged.schema, &schema_content)?;
        let description_content =
            serde_json::to_string_pretty(&description).context("序列化 bot 描述失败")?;
        write_new_file(&staged.description, &description_content)?;
        let version_content = crate::version::installed_version_json(manifest)?;
        write_new_file(&staged.version, &version_content)?;

        let replacements = [
            FileReplacement {
                target: files.artifact.clone(),
                staged: staged.artifact.clone(),
                backup: backups.artifact,
            },
            FileReplacement {
                target: files.schema.clone(),
                staged: staged.schema.clone(),
                backup: backups.schema,
            },
            FileReplacement {
                target: files.description.clone(),
                staged: staged.description.clone(),
                backup: backups.description,
            },
            FileReplacement {
                target: files.version.clone(),
                staged: staged.version.clone(),
                backup: backups.version,
            },
        ];
        replace_files(&replacements)
    }

    /// 扫描本地已安装的制品（`~/.tiangong/bots/*/`）。
    ///
    /// 找出有 bot 二进制 + schema.json 的目录，返回本地制品列表。
    /// 不依赖线上 bots-index——即使未发布 index，本地已放置的制品也能
    /// 被 UI 发现并注册。
    pub fn scan_local_artifacts(&self) -> Vec<LocalArtifact> {
        scan_local_artifacts_impl()
    }

    /// 检查某制品是否有线上更新。
    ///
    /// 拉取 bots-index，对比线上版本与本地已安装版本。
    /// 返回 `Some(manifest)` 表示有更新，`None` 表示已是最新。
    pub async fn check_update(&self, artifact_id: &str) -> Result<Option<BotManifest>> {
        let index = self.downloader.fetch_index().await?;
        let manifest = index
            .bots
            .into_iter()
            .find(|m| m.id == artifact_id)
            .ok_or_else(|| anyhow!("bots-index 中未找到制品：{artifact_id}"))?;
        // 查找本地已安装版本：遍历 bots 目录，找 artifact_id 匹配的 version.json。
        let local = find_local_version(artifact_id);
        if crate::version::has_update(local.as_ref(), &manifest.version) {
            Ok(Some(manifest))
        } else {
            Ok(None)
        }
    }

    /// 升级 bot 制品（停止运行中的 bot → 下载新版本 → 写 version）。
    ///
    /// 本层只负责停止和替换制品，是否恢复运行由调用方按升级前状态决定。
    pub async fn upgrade(
        &self,
        bot_id: &BotId,
        manifest: BotManifest,
        progress: Option<ProgressFn>,
    ) -> Result<()> {
        self.stop(bot_id).await?;
        // 下载安装（复用 install 逻辑，含 schema 缓存 + version 写入）。
        self.install(manifest, bot_id, progress).await?;
        Ok(())
    }

    /// 拉取远端 bots-index.json。
    pub async fn fetch_index(&self) -> Result<crate::manifest::BotsIndex> {
        self.downloader.fetch_index().await
    }

    /// 调用指定 bot 制品创建扫码配置会话。
    pub async fn provision_begin(&self, bot_id: &BotId) -> Result<crate::QrSession> {
        paths::ensure_executable_paths_safe(bot_id)?;
        crate::provision::begin(&paths::bot_artifact_path(bot_id)).await
    }

    /// 调用指定 bot 制品轮询扫码配置状态。
    pub async fn provision_poll(
        &self,
        bot_id: &BotId,
        session: &crate::QrSession,
    ) -> Result<crate::ProvisionStatus> {
        paths::ensure_executable_paths_safe(bot_id)?;
        crate::provision::poll(&paths::bot_artifact_path(bot_id), session).await
    }

    /// 判断 Bot 是否声明了 MCP 能力。
    pub async fn supports_mcp(&self, bot_id: &BotId) -> Result<bool> {
        let description = match cached_description(bot_id) {
            Some(description) => description,
            None => describe_and_cache_full(bot_id).await?,
        };
        Ok(description.capabilities.mcp.is_some())
    }

    /// 读取 Bot 已发现的推送目标。
    pub async fn push_targets(&self, bot_id: &BotId) -> Result<Vec<PushTargetView>> {
        ensure_mcp_capability(bot_id).await?;
        let output = run_management_command(bot_id, &["--push-target-list"], None).await?;
        let parsed: PushTargetList =
            serde_json::from_slice(&output.stdout).context("解析 Bot 推送目标列表失败")?;
        Ok(parsed.targets)
    }

    /// 删除一个 Bot 推送授权目标。
    pub async fn delete_push_target(&self, bot_id: &BotId, target_id: &str) -> Result<()> {
        ensure_mcp_capability(bot_id).await?;
        let target_id = target_id.trim();
        if target_id.is_empty() {
            bail!("推送目标 ID 不能为空");
        }
        let input = serde_json::to_vec(&serde_json::json!({
            "target_id": target_id,
        }))
        .context("序列化推送目标删除请求失败")?;
        run_management_command(bot_id, &["--push-target-delete"], Some(input.as_slice())).await?;
        Ok(())
    }

    /// 执行 `bot --mcp generate` 并校验其普通 MCP 注册配置。
    pub async fn generate_mcp_config(&self, bot_id: &BotId) -> Result<BotMcpConfig> {
        ensure_mcp_capability(bot_id).await?;
        let output = run_management_command(bot_id, &["--mcp", "generate"], None).await?;
        let config: BotMcpConfig =
            serde_json::from_slice(&output.stdout).context("解析 Bot MCP 配置失败")?;
        validate_generated_mcp_config(bot_id, &config)?;
        Ok(config)
    }

    /// 启动指定 bot 实例（需制品已安装）。
    ///
    /// 启动时按缓存的 schema（`bot --describe` 上报）校验必填字段，
    /// 并按 schema 的 `env` 映射注入环境变量。`extra_env` 提供主程序注入的
    /// 额外环境变量（如 `TIANGONG_URL`/`TIANGONG_TOKEN`，由 ServerConfig 推导），
    /// 覆盖同名 schema 映射。
    /// 启动 bot（PID-based 独立运行，issue #286 方案）。
    ///
    /// spawn 子进程（脱离会话）、写 PID 文件、不持有句柄、不自动重启。
    /// bot 崩溃即停止（PID 文件残留失效，下次 health 清理）。已在运行则拒绝重复启动。
    pub async fn start(
        &self,
        config: &BotConfig,
        extra_env: &BTreeMap<String, String>,
    ) -> Result<()> {
        if crate::pid::is_running(&config.id) {
            return Err(anyhow!("bot 已在运行：{}", config.id));
        }
        let artifact = paths::bot_artifact_path(&config.id);
        if !artifact.exists() {
            return Err(anyhow!(
                "bot 制品未安装，请先安装：{}（{}）",
                config.id,
                artifact.display()
            ));
        }
        let schema = cached_schema(&config.id).unwrap_or_default();
        crate::management::validate_bot_config_fields(&schema, &config.config)?;
        let mut env = bot_env(config, &schema);
        env.extend(extra_env.clone());
        spawn_detached(&config.id, &artifact, &env)?;
        Ok(())
    }

    /// 停止指定 bot 实例。
    /// 停止 bot（PID-based：读 PID → SIGTERM → 等待 → 清理）。
    pub async fn stop(&self, id: &BotId) -> Result<()> {
        crate::pid::stop_bot(id)
    }

    /// 停止所有运行中的 bot。
    /// 停止所有 bot（遍历 store 配置的 bot，逐个 PID-based stop）。
    pub async fn stop_all(&self) {
        for bot in self.store.list() {
            if let Err(err) = crate::pid::stop_bot(&bot.id) {
                tracing::warn!("停止 bot {} 失败：{err}", bot.id);
            }
        }
    }

    /// 查询 bot 是否正在由运行时监督；已结束条目会被清理。
    /// bot 是否正在运行（PID 文件存在且进程存活）。
    pub async fn is_running(&self, id: &BotId) -> bool {
        crate::pid::is_running(id)
    }

    /// 查询 bot 健康状态。
    ///
    /// 若 bot 正常退出（监督任务结束），自动从运行表移除并返回 Stopped，
    /// 避免 health 长期误报 Running。
    /// bot 健康状态（PID-based）。
    pub async fn health(&self, id: &BotId) -> BotHealth {
        if crate::pid::is_running(id) {
            BotHealth::Running
        } else if paths::bot_artifact_path(id).exists() {
            BotHealth::Stopped
        } else {
            BotHealth::MissingArtifact
        }
    }

    /// 启动所有 enabled 且已安装制品的 bot（主程序启动时调用）。
    ///
    /// `extra_env` 为注入所有 bot 的额外环境变量（如 TIANGONG_URL/TIANGONG_TOKEN）。
    /// 启动所有 enabled 的 bot（跳过 PID 已存活的，避免重复启动）。
    pub async fn start_enabled(&self, extra_env: &BTreeMap<String, String>) {
        let bots = self.store.list();
        for bot in bots.iter().filter(|b| b.enabled) {
            if crate::pid::is_running(&bot.id) {
                tracing::info!("bot {} 已在运行（PID 存活），跳过", bot.id);
                continue;
            }
            if paths::bot_artifact_path(&bot.id).exists() {
                if let Err(err) = self.start(bot, extra_env).await {
                    tracing::warn!("启动 bot {} 失败：{err}", bot.id);
                }
            } else {
                tracing::info!("bot {} 已启用但制品未安装，跳过自动启动", bot.id);
            }
        }
    }
}

/// 按 schema 的 `env` 字段注入环境变量。
///
/// schema 是 bot 二进制 `--describe` 上报的权威配置描述，每个字段声明了
/// 它对应的环境变量名。主程序据此注入，取代硬编码映射。
pub fn bot_env(config: &BotConfig, schema: &[ConfigFieldSchema]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for field in schema {
        if let (Some(var), Some(val)) = (&field.env, config.config_string(&field.key)) {
            env.insert(var.clone(), val);
        }
    }
    env
}

/// 调用 `bot --describe` 获取 schema 并缓存到 `schema.json`。
///
/// bot 二进制收到 `--describe` 参数后，输出 schema JSON 到 stdout 并退出。
/// 用于没有远端清单和版本记录的本地 bot；其 `artifact_id` 必须与目录 ID 一致。
pub async fn describe_and_cache(bot_id: &BotId) -> Result<Vec<ConfigFieldSchema>> {
    Ok(describe_and_cache_full(bot_id).await?.config_schema)
}

async fn describe_and_cache_full(bot_id: &BotId) -> Result<BotDescription> {
    paths::ensure_executable_paths_safe(bot_id)?;
    let artifact_path = paths::bot_artifact_path(bot_id);
    let parsed = describe_artifact(&artifact_path).await?;
    validate_local_description(&parsed, bot_id)?;
    cache_description(bot_id, &parsed)?;
    Ok(parsed)
}

fn cache_description(bot_id: &BotId, description: &BotDescription) -> Result<()> {
    let schema_path = paths::bot_schema_path(bot_id);
    let schema_content = serde_json::to_string_pretty(&description.config_schema)
        .context("序列化 bot schema 失败")?;
    crate::store::atomic_write(&schema_path, &schema_content)
        .with_context(|| format!("写入 schema 缓存失败：{}", schema_path.display()))?;

    let description_path = paths::bot_description_path(bot_id);
    let description_content =
        serde_json::to_string_pretty(description).context("序列化 bot 描述失败")?;
    crate::store::atomic_write(&description_path, &description_content)
        .with_context(|| format!("写入 Bot 描述缓存失败：{}", description_path.display()))
}

fn validate_local_description(description: &BotDescription, bot_id: &BotId) -> Result<()> {
    if description.schema_version != 1 {
        bail!(
            "bot --describe schema_version 不受支持：{}",
            description.schema_version
        );
    }
    if description.artifact_id != bot_id.as_str() {
        bail!(
            "本地 bot --describe artifact_id 与目录名不一致：期望 {}，实际 {}",
            bot_id,
            description.artifact_id
        );
    }
    validate_capabilities(description)
}

async fn describe_artifact(artifact_path: &Path) -> Result<BotDescription> {
    use tokio::process::Command;
    paths::reject_symlink(artifact_path, "Bot 制品")?;
    let mut cmd = Command::new(artifact_path);
    cmd.arg("--describe");
    tiangong_types::process::configure_tokio_no_window(&mut cmd);
    let output = cmd
        .output()
        .await
        .with_context(|| format!("执行 bot --describe 失败：{}", artifact_path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "bot --describe 退出码非零：{} stderr={}",
            output.status,
            stderr.chars().take(1024).collect::<String>()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).with_context(|| {
        format!(
            "解析 bot --describe 输出失败：{}",
            stdout.chars().take(512).collect::<String>()
        )
    })
}

fn validate_description(description: &BotDescription, manifest: &BotManifest) -> Result<()> {
    if description.schema_version != 1 {
        bail!(
            "bot --describe schema_version 不受支持：{}",
            description.schema_version
        );
    }
    if description.artifact_id != manifest.id {
        bail!(
            "bot --describe artifact_id 与清单不一致：期望 {}，实际 {}",
            manifest.id,
            description.artifact_id
        );
    }
    validate_capabilities(description)
}

fn validate_capabilities(description: &BotDescription) -> Result<()> {
    let Some(capability) = description.capabilities.mcp.as_ref() else {
        return Ok(());
    };
    if capability.protocol_version != 1 {
        bail!(
            "bot mcp protocol_version 不受支持：{}",
            capability.protocol_version
        );
    }
    Ok(())
}

async fn ensure_mcp_capability(bot_id: &BotId) -> Result<BotDescription> {
    let description = match cached_description(bot_id) {
        Some(description) => description,
        None => describe_and_cache_full(bot_id).await?,
    };
    if description.capabilities.mcp.is_none() {
        bail!("该 Bot 不支持 MCP");
    }
    Ok(description)
}

fn validate_generated_mcp_config(bot_id: &BotId, config: &BotMcpConfig) -> Result<()> {
    if config.schema_version != 1 {
        bail!("Bot MCP 配置版本不受支持：{}", config.schema_version);
    }
    let expected_name = format!("bot-{bot_id}");
    if config.name != expected_name {
        bail!(
            "Bot MCP 名称无效：期望 {expected_name}，实际 {}",
            config.name
        );
    }
    if config.transport != "stdio" {
        bail!("Bot MCP 仅支持 stdio transport");
    }
    if config.args != ["--mcp"] {
        bail!("Bot MCP 启动参数必须为 --mcp");
    }
    if !config.enabled {
        bail!("Bot MCP 生成配置必须默认启用");
    }
    let expected_command = std::fs::canonicalize(paths::bot_artifact_path(bot_id))
        .context("解析已安装 Bot 制品路径失败")?;
    let generated_command =
        std::fs::canonicalize(&config.command).context("解析 Bot 生成的 MCP 命令路径失败")?;
    if generated_command != expected_command {
        bail!("Bot MCP 配置试图注册非当前制品命令");
    }
    Ok(())
}

async fn run_management_command(
    bot_id: &BotId,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> Result<std::process::Output> {
    use tokio::process::Command;

    paths::ensure_executable_paths_safe(bot_id)?;
    let artifact = paths::bot_artifact_path(bot_id);
    let command_label = arguments.join(" ");
    let mut command = Command::new(&artifact);
    command
        .args(arguments)
        .current_dir(paths::bot_runtime_dir(bot_id))
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    tiangong_types::process::configure_tokio_no_window(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("启动 Bot 管理命令失败：{command_label}"))?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().context("打开 Bot 管理命令 stdin 失败")?;
        stdin
            .write_all(input)
            .await
            .context("写入 Bot 管理命令 stdin 失败")?;
    }
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .map_err(|_| anyhow!("Bot 管理命令超时：{command_label}"))??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Bot 管理命令失败：{command_label} status={} stderr={}",
            output.status,
            stderr.chars().take(1024).collect::<String>()
        );
    }
    Ok(output)
}

fn transaction_path(target: &Path, purpose: &str, transaction_id: &str) -> PathBuf {
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    target.with_file_name(format!(".{purpose}-{transaction_id}-{file_name}"))
}

fn write_new_file(path: &Path, content: &str) -> Result<()> {
    paths::reject_symlink(path, "Bot 升级临时文件")?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("创建升级临时文件失败：{}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("写入升级临时文件失败：{}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("同步升级临时文件失败：{}", path.display()))?;
    Ok(())
}

fn replace_files(replacements: &[FileReplacement]) -> Result<()> {
    for replacement in replacements {
        paths::reject_symlink(&replacement.target, "Bot 安装文件")?;
        paths::reject_symlink(&replacement.staged, "Bot 升级临时文件")?;
        paths::reject_symlink(&replacement.backup, "Bot 升级备份文件")?;
        if replacement.backup.exists() {
            bail!("升级备份文件已存在：{}", replacement.backup.display());
        }
    }

    let mut backed_up = vec![false; replacements.len()];
    for (index, replacement) in replacements.iter().enumerate() {
        if replacement.target.exists() {
            if let Err(error) = std::fs::rename(&replacement.target, &replacement.backup) {
                let recovery_errors = rollback_files(replacements, &backed_up, 0);
                return Err(replacement_error(
                    format!(
                        "备份旧文件失败：{} -> {}：{error}",
                        replacement.target.display(),
                        replacement.backup.display()
                    ),
                    recovery_errors,
                ));
            }
            backed_up[index] = true;
        }
    }

    for (installed, replacement) in replacements.iter().enumerate() {
        if let Err(error) = std::fs::rename(&replacement.staged, &replacement.target) {
            let recovery_errors = rollback_files(replacements, &backed_up, installed);
            return Err(replacement_error(
                format!(
                    "替换新文件失败：{} -> {}：{error}",
                    replacement.staged.display(),
                    replacement.target.display()
                ),
                recovery_errors,
            ));
        }
    }

    for (replacement, had_original) in replacements.iter().zip(backed_up) {
        if had_original && let Err(error) = std::fs::remove_file(&replacement.backup) {
            tracing::warn!(
                "清理 bot 升级备份失败：{} error={error}",
                replacement.backup.display()
            );
        }
    }
    Ok(())
}

fn rollback_files(
    replacements: &[FileReplacement],
    backed_up: &[bool],
    installed: usize,
) -> Vec<String> {
    let mut errors = Vec::new();
    for replacement in replacements[..installed].iter().rev() {
        if replacement.target.exists()
            && let Err(error) = std::fs::remove_file(&replacement.target)
        {
            errors.push(format!(
                "移除新文件 {} 失败：{error}",
                replacement.target.display()
            ));
        }
    }
    for (replacement, had_original) in replacements.iter().zip(backed_up).rev() {
        if *had_original
            && let Err(error) = std::fs::rename(&replacement.backup, &replacement.target)
        {
            errors.push(format!(
                "恢复旧文件 {} 失败：{error}",
                replacement.target.display()
            ));
        }
    }
    errors
}

fn replacement_error(error: String, recovery_errors: Vec<String>) -> anyhow::Error {
    if recovery_errors.is_empty() {
        anyhow!("{error}；旧制品已恢复")
    } else {
        anyhow!("{error}；旧制品恢复失败：{}", recovery_errors.join("；"))
    }
}

/// 读取缓存的 schema（`bot --describe` 上报的结果）。
///
/// 用于表单渲染、必填校验、环境变量注入。返回 `None` 表示尚未安装制品
/// 或尚未执行 describe。
pub fn cached_schema(bot_id: &BotId) -> Option<Vec<ConfigFieldSchema>> {
    let path = paths::bot_schema_path(bot_id);
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// 读取 `description.json` 中缓存的完整 Bot 描述。
pub fn cached_description(bot_id: &BotId) -> Option<BotDescription> {
    let path = paths::bot_description_path(bot_id);
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// 在 `~/.tiangong/bots/` 下查找指定 artifact_id 的已安装版本。
///
/// 一个 artifact_id 可能对应多个 bot 实例（如两个 feishu bot），
/// 返回找到的第一个。用于 `check_update` 时对比本地版本。
fn find_local_version(artifact_id: &str) -> Option<crate::version::InstalledVersion> {
    let bots_dir = paths::default_bots_dir();
    let entries = std::fs::read_dir(&bots_dir).ok()?;
    for entry in entries.flatten() {
        let Some(id) = local_directory_id(&entry) else {
            continue;
        };
        let version = crate::version::read_installed_version(&id);
        if let Some(ref v) = version
            && v.artifact_id == artifact_id
        {
            return version;
        }
    }
    None
}

/// 扫描 `~/.tiangong/bots/*/`，返回有 bot 二进制 + schema.json 的本地制品。
fn scan_local_artifacts_impl() -> Vec<LocalArtifact> {
    let bots_dir = paths::default_bots_dir();
    let entries = match std::fs::read_dir(&bots_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut artifacts = Vec::new();
    for entry in entries.flatten() {
        let bot_id = match local_directory_id(&entry) {
            Some(id) => id,
            None => continue,
        };
        if let Err(error) = paths::ensure_executable_paths_safe(&bot_id) {
            tracing::warn!("跳过不安全的 Bot 本地目录：{error}");
            continue;
        }
        let bot_binary = paths::bot_artifact_path(&bot_id);
        if !bot_binary.exists() {
            continue;
        }
        // 读取 schema 缓存。
        let schema = cached_schema(&bot_id).unwrap_or_default();
        let supports_mcp = cached_description(&bot_id)
            .and_then(|description| description.capabilities.mcp)
            .is_some();
        // 读取版本记录（推断 artifact_id）。
        let version_info = crate::version::read_installed_version(&bot_id);
        let artifact_id = version_info
            .as_ref()
            .map(|v| v.artifact_id.clone())
            // 无 version.json 时，回退用 schema 的 artifact_id 或目录名。
            .or_else(|| {
                // describe 缓存的 schema.json 只有数组，不含 artifact_id；
                // 回退到目录名。
                Some(bot_id.as_str().to_string())
            })
            .unwrap_or_else(|| bot_id.as_str().to_string());
        let name = version_info
            .as_ref()
            .map(|v| v.name.trim())
            .filter(|name| !name.is_empty())
            .unwrap_or(&artifact_id)
            .to_string();
        let version = version_info
            .as_ref()
            .map(|v| v.version.clone())
            .unwrap_or_default();
        artifacts.push(LocalArtifact {
            id: bot_id.as_str().to_string(),
            name,
            artifact_id,
            version,
            config_schema: schema,
            supports_mcp,
        });
    }
    artifacts
}

fn local_directory_id(entry: &std::fs::DirEntry) -> Option<BotId> {
    let file_type = match entry.file_type() {
        Ok(file_type) => file_type,
        Err(error) => {
            tracing::warn!(
                "读取 Bot 本地目录类型失败：{} error={error}",
                entry.path().display()
            );
            return None;
        }
    };
    if file_type.is_symlink() {
        tracing::warn!("跳过符号链接 Bot 本地目录：{}", entry.path().display());
        return None;
    }
    if !file_type.is_dir() {
        return None;
    }

    let file_name = entry.file_name();
    let Some(raw_id) = file_name.to_str() else {
        tracing::warn!(
            "跳过名称不是 UTF-8 的 Bot 本地目录：{}",
            entry.path().display()
        );
        return None;
    };
    match BotId::try_from(raw_id) {
        Ok(id) => Some(id),
        Err(error) => {
            tracing::warn!("跳过非法 Bot 本地目录：{error}");
            None
        }
    }
}

/// 后台 spawn bot 子进程（脱离会话、不随父进程退出、写 PID 文件）。
fn spawn_detached(
    bot_id: &BotId,
    artifact_path: &Path,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    use std::process::Command;
    paths::ensure_executable_paths_safe(bot_id)?;
    paths::reject_symlink(artifact_path, "Bot 制品")?;
    let mut cmd = Command::new(artifact_path);
    tiangong_types::process::configure_no_window(&mut cmd);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                let _ = libc::setsid();
                Ok(())
            });
        }
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn bot 失败：{}", artifact_path.display()))?;
    let pid = child.id();
    std::mem::forget(child);
    crate::pid::write_pid(bot_id, pid)?;
    tracing::info!("bot 已后台启动：{} pid={pid}", bot_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FieldType;
    use crate::manifest::{BotArtifact, current_platform_key};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn feishu_schema() -> Vec<ConfigFieldSchema> {
        vec![
            ConfigFieldSchema {
                key: "app_id".into(),
                label: "App ID".into(),
                field_type: FieldType::Barcode,
                required: true,
                env: Some("TIANGONG_BOT_FEISHU_APP_ID".into()),
                default: None,
                help: None,
            },
            ConfigFieldSchema {
                key: "app_secret".into(),
                label: "App Secret".into(),
                field_type: FieldType::Barcode,
                required: true,
                env: Some("TIANGONG_BOT_FEISHU_APP_SECRET".into()),
                default: None,
                help: None,
            },
            ConfigFieldSchema {
                key: "tiangong_url".into(),
                label: "天工服务地址".into(),
                field_type: FieldType::String,
                required: false,
                env: Some("TIANGONG_URL".into()),
                default: None,
                help: None,
            },
        ]
    }

    fn feishu_bot() -> BotConfig {
        let mut config = BTreeMap::new();
        config.insert("app_id".into(), serde_json::json!("cli_test"));
        config.insert("app_secret".into(), serde_json::json!("secret"));
        config.insert(
            "tiangong_url".into(),
            serde_json::json!("http://127.0.0.1:9090"),
        );
        BotConfig {
            id: BotId::try_from("test").unwrap(),
            artifact_id: "feishu".into(),
            enabled: true,
            config,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    #[test]
    fn bot_env_from_schema() {
        let bot = feishu_bot();
        let env = bot_env(&bot, &feishu_schema());
        assert_eq!(
            env.get("TIANGONG_BOT_FEISHU_APP_ID"),
            Some(&"cli_test".to_string())
        );
        assert_eq!(
            env.get("TIANGONG_BOT_FEISHU_APP_SECRET"),
            Some(&"secret".to_string())
        );
        assert_eq!(
            env.get("TIANGONG_URL"),
            Some(&"http://127.0.0.1:9090".to_string())
        );
    }

    #[test]
    fn bot_env_omits_missing_fields() {
        let mut config = BTreeMap::new();
        config.insert("app_id".into(), serde_json::json!("cli_test"));
        let bot = BotConfig {
            id: BotId::try_from("partial").unwrap(),
            artifact_id: "feishu".into(),
            enabled: false,
            config,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        let env = bot_env(&bot, &feishu_schema());
        assert!(env.contains_key("TIANGONG_BOT_FEISHU_APP_ID"));
        assert!(!env.contains_key("TIANGONG_BOT_FEISHU_APP_SECRET"));
        assert!(!env.contains_key("TIANGONG_URL"));
    }

    #[test]
    fn bot_env_ignores_fields_without_env_mapping() {
        // schema 里某字段没有 env 映射 → 不注入。
        let schema = vec![ConfigFieldSchema {
            key: "display_only".into(),
            label: "仅展示".into(),
            field_type: FieldType::String,
            required: false,
            env: None,
            default: None,
            help: None,
        }];
        let mut config = BTreeMap::new();
        config.insert("display_only".into(), serde_json::json!("value"));
        let bot = BotConfig {
            id: BotId::try_from("t").unwrap(),
            artifact_id: "feishu".into(),
            enabled: false,
            config,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        let env = bot_env(&bot, &schema);
        assert!(env.is_empty());
    }

    #[test]
    fn local_directory_filter_skips_invalid_ids() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("feishu")).unwrap();
        std::fs::create_dir(root.path().join("1feishu")).unwrap();
        std::fs::create_dir(root.path().join("feishu-bot")).unwrap();
        std::fs::create_dir(root.path().join("con")).unwrap();

        let mut accepted = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter_map(|entry| local_directory_id(&entry))
            .map(|id| id.as_str().to_string())
            .collect::<Vec<_>>();
        accepted.sort();
        assert_eq!(accepted, vec!["feishu"]);
    }

    #[cfg(unix)]
    #[test]
    fn local_directory_filter_skips_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, root.path().join("feishu")).unwrap();
        let entry = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .find(|entry| entry.file_name() == "feishu")
            .unwrap();

        assert!(local_directory_id(&entry).is_none());
    }

    #[tokio::test]
    async fn stop_is_idempotent_when_bot_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(BotStore::with_config_path(dir.path().join("bots.json")).unwrap());
        let runtime = BotRuntime::new(store).unwrap();
        let id = BotId::try_from("stopped").unwrap();

        runtime.stop(&id).await.unwrap();
        runtime.stop(&id).await.unwrap();
    }

    #[test]
    fn replacement_failure_restores_all_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let files = ArtifactFiles {
            artifact: dir.path().join("bot"),
            schema: dir.path().join("schema.json"),
            description: dir.path().join("description.json"),
            version: dir.path().join("version.json"),
        };
        for (path, content) in files.all().into_iter().zip([
            b"old artifact".as_slice(),
            b"old schema".as_slice(),
            b"old description".as_slice(),
            b"old version".as_slice(),
        ]) {
            std::fs::write(path, content).unwrap();
        }

        let staged = files.transaction_files("download", "test");
        let backups = files.transaction_files("backup", "test");
        let _staged_cleanup = StagedFiles {
            files: staged.clone(),
        };
        std::fs::write(&staged.artifact, b"new artifact").unwrap();
        std::fs::write(&staged.version, b"new version").unwrap();
        let replacements = [
            FileReplacement {
                target: files.artifact.clone(),
                staged: staged.artifact,
                backup: backups.artifact.clone(),
            },
            FileReplacement {
                target: files.schema.clone(),
                staged: staged.schema,
                backup: backups.schema.clone(),
            },
            FileReplacement {
                target: files.description.clone(),
                staged: staged.description,
                backup: backups.description.clone(),
            },
            FileReplacement {
                target: files.version.clone(),
                staged: staged.version,
                backup: backups.version.clone(),
            },
        ];

        let error = replace_files(&replacements).unwrap_err();
        assert!(format!("{error}").contains("旧制品已恢复"));
        assert_eq!(std::fs::read(&files.artifact).unwrap(), b"old artifact");
        assert_eq!(std::fs::read(&files.schema).unwrap(), b"old schema");
        assert_eq!(
            std::fs::read(&files.description).unwrap(),
            b"old description"
        );
        assert_eq!(std::fs::read(&files.version).unwrap(), b"old version");
        assert!(backups.all().into_iter().all(|path| !path.exists()));
    }

    #[test]
    fn successful_replacement_updates_all_files_and_removes_backups() {
        let dir = tempfile::tempdir().unwrap();
        let files = ArtifactFiles {
            artifact: dir.path().join("bot"),
            schema: dir.path().join("schema.json"),
            description: dir.path().join("description.json"),
            version: dir.path().join("version.json"),
        };
        let staged = files.transaction_files("download", "success");
        let backups = files.transaction_files("backup", "success");
        for (path, content) in files.all().into_iter().zip([
            b"old artifact".as_slice(),
            b"old schema".as_slice(),
            b"old description".as_slice(),
            b"old version".as_slice(),
        ]) {
            std::fs::write(path, content).unwrap();
        }
        for (path, content) in staged.all().into_iter().zip([
            b"new artifact".as_slice(),
            b"new schema".as_slice(),
            b"new description".as_slice(),
            b"new version".as_slice(),
        ]) {
            std::fs::write(path, content).unwrap();
        }
        let replacements = [
            FileReplacement {
                target: files.artifact.clone(),
                staged: staged.artifact,
                backup: backups.artifact.clone(),
            },
            FileReplacement {
                target: files.schema.clone(),
                staged: staged.schema,
                backup: backups.schema.clone(),
            },
            FileReplacement {
                target: files.description.clone(),
                staged: staged.description,
                backup: backups.description.clone(),
            },
            FileReplacement {
                target: files.version.clone(),
                staged: staged.version,
                backup: backups.version.clone(),
            },
        ];

        replace_files(&replacements).unwrap();

        assert_eq!(std::fs::read(&files.artifact).unwrap(), b"new artifact");
        assert_eq!(std::fs::read(&files.schema).unwrap(), b"new schema");
        assert_eq!(
            std::fs::read(&files.description).unwrap(),
            b"new description"
        );
        assert_eq!(std::fs::read(&files.version).unwrap(), b"new version");
        assert!(backups.all().into_iter().all(|path| !path.exists()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_new_artifact_keeps_existing_installation() {
        let server = MockServer::start().await;
        let payload = br###"#!/bin/sh
printf '%s\n' '{"schema_version":1,"artifact_id":"other","config_schema":[]}'
"###;
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let checksum = format!("sha256:{}", hex::encode(hasher.finalize()));
        Mock::given(method("GET"))
            .and(path("/artifact"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.to_vec()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("bot-files");
        std::fs::create_dir_all(&install_dir).unwrap();
        let files = ArtifactFiles {
            artifact: install_dir.join("bot"),
            schema: install_dir.join("schema.json"),
            description: install_dir.join("description.json"),
            version: install_dir.join("version.json"),
        };
        for (path, content) in files.all().into_iter().zip([
            b"old artifact".as_slice(),
            b"old schema".as_slice(),
            b"old description".as_slice(),
            b"old version".as_slice(),
        ]) {
            std::fs::write(path, content).unwrap();
        }

        let mut platforms = BTreeMap::new();
        platforms.insert(
            current_platform_key(),
            BotArtifact {
                url: format!("{}/artifact", server.uri()),
                checksum,
            },
        );
        let manifest = BotManifest {
            id: "feishu".into(),
            name: "Feishu".into(),
            version: "2.0.0".into(),
            description: String::new(),
            config_schema: Vec::new(),
            platforms,
            min_app_version: None,
        };
        let store = Arc::new(BotStore::with_config_path(dir.path().join("bots.json")).unwrap());
        let runtime = BotRuntime::new(store).unwrap();

        let error = runtime
            .install_to_files(&manifest, &files, None)
            .await
            .unwrap_err();
        assert!(format!("{error}").contains("artifact_id 与清单不一致"));
        assert_eq!(std::fs::read(&files.artifact).unwrap(), b"old artifact");
        assert_eq!(std::fs::read(&files.schema).unwrap(), b"old schema");
        assert_eq!(std::fs::read(&files.version).unwrap(), b"old version");
        let leftovers = std::fs::read_dir(&install_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "残留事务文件：{leftovers:?}");
    }
}
