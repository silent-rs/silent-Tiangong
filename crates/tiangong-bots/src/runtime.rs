//! bot 运行时——按 bot 实例启停、健康检查的运行时表。
//!
//! 组合 [`crate::store`] 的配置与 [`crate::supervisor`] 的进程监督。
//! bot 凭证由 [`BotConfig::config`] 转成环境变量注入子进程（飞书用
//! `TIANGONG_BOT_FEISHU_*` 前缀，见 [`bot_env`])。

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::{BotConfig, ConfigFieldSchema};
use crate::downloader::{Downloader, ProgressFn};
use crate::management::BotStore;
use crate::manifest::BotManifest;
use crate::paths;
use crate::supervisor::SupervisedBot;

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

/// 被运行时管理的 bot 条目。
struct RuntimeEntry {
    supervised: SupervisedBot,
}

/// bot 运行时表。
pub struct BotRuntime {
    store: Arc<BotStore>,
    downloader: Downloader,
    entries: Mutex<HashMap<String, RuntimeEntry>>,
    /// 制品 manifest 缓存（artifact_id → manifest），由安装时填入。
    manifests: Mutex<HashMap<String, BotManifest>>,
}

impl BotRuntime {
    pub fn new(store: Arc<BotStore>) -> Result<Self> {
        Ok(Self {
            store,
            downloader: Downloader::new()?,
            entries: Mutex::new(HashMap::new()),
            manifests: Mutex::new(HashMap::new()),
        })
    }

    /// 安装（下载 + 校验）某制品到 bot 实例目录。
    ///
    /// 安装成功后自动调用 `bot --describe` 获取并缓存 config schema。
    pub async fn install(
        &self,
        manifest: BotManifest,
        dest_id: &str,
        progress: Option<ProgressFn>,
    ) -> Result<PathBuf> {
        let artifact_id = manifest.id.clone();
        let path = self
            .downloader
            .install_artifact(&manifest, dest_id, progress)
            .await?;
        self.manifests.lock().await.insert(artifact_id, manifest);
        // 获取并缓存 schema（失败仅警告，不阻断安装）。
        if let Err(err) = describe_and_cache(&path, dest_id).await {
            tracing::warn!("获取 bot schema 失败（{}）：{err}", path.display());
        }
        Ok(path)
    }

    /// 拉取远端 bots-index.json。
    pub async fn fetch_index(&self) -> Result<crate::manifest::BotsIndex> {
        self.downloader.fetch_index().await
    }

    /// 启动指定 bot 实例（需制品已安装）。
    ///
    /// 启动时按缓存的 schema（`bot --describe` 上报）校验必填字段，
    /// 并按 schema 的 `env` 映射注入环境变量。
    pub async fn start(&self, config: &BotConfig) -> Result<()> {
        let mut entries = self.entries.lock().await;
        // 清理已结束的残留条目，避免误报"已在运行"。
        if let Some(entry) = entries.get(&config.id)
            && entry.supervised.is_finished()
        {
            entries.remove(&config.id);
        }
        if entries.contains_key(&config.id) {
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

        // 按缓存 schema 校验必填字段。
        let schema = cached_schema(&config.id).unwrap_or_default();
        crate::management::validate_bot_config_fields(&schema, &config.config)?;

        let env = bot_env(config, &schema);
        let supervised = crate::supervisor::spawn_supervised(&config.id, artifact, env)
            .context("启动 bot 进程失败")?;
        entries.insert(config.id.clone(), RuntimeEntry { supervised });
        Ok(())
    }

    /// 停止指定 bot 实例。
    pub async fn stop(&self, id: &str) -> Result<()> {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.remove(id) {
            entry.supervised.stop().await;
            Ok(())
        } else {
            Err(anyhow!("bot 未在运行：{id}"))
        }
    }

    /// 停止所有运行中的 bot。
    pub async fn stop_all(&self) {
        let mut entries = self.entries.lock().await;
        let drained: Vec<(String, RuntimeEntry)> = entries.drain().collect();
        drop(entries);
        for (_, entry) in drained {
            entry.supervised.stop().await;
        }
    }

    /// 查询 bot 健康状态。
    ///
    /// 若 bot 正常退出（监督任务结束），自动从运行表移除并返回 Stopped，
    /// 避免 health 长期误报 Running。
    pub async fn health(&self, id: &str) -> BotHealth {
        let mut entries = self.entries.lock().await;
        let running = match entries.get(id) {
            Some(entry) if !entry.supervised.is_finished() => true,
            Some(_) => {
                // 监督任务已结束（bot 退出），清理运行表条目。
                entries.remove(id);
                false
            }
            None => false,
        };
        drop(entries);
        if running {
            BotHealth::Running
        } else if paths::bot_artifact_path(id).exists() {
            BotHealth::Stopped
        } else {
            BotHealth::MissingArtifact
        }
    }

    /// 启动所有 enabled 且已安装制品的 bot（主程序启动时调用）。
    pub async fn start_enabled(&self) {
        let bots = self.store.list();
        for bot in bots.iter().filter(|b| b.enabled) {
            if paths::bot_artifact_path(&bot.id).exists() {
                if let Err(err) = self.start(bot).await {
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

/// bot `--describe` 上报的顶层结构。
#[derive(Debug, Deserialize)]
struct DescribeOutput {
    /// schema 格式版本。
    #[allow(dead_code)]
    schema_version: u32,
    /// 制品 id（标识 bot 平台，与 manifest 的 id 一致）。
    #[allow(dead_code)]
    #[serde(default)]
    artifact_id: String,
    /// 配置字段 schema。
    config_schema: Vec<ConfigFieldSchema>,
}

/// 调用 `bot --describe` 获取 schema 并缓存到 `schema.json`。
///
/// bot 二进制收到 `--describe` 参数后，输出 schema JSON 到 stdout 并退出。
/// 主程序在 [`BotRuntime::install`] 成功后调用此函数缓存 schema。
pub async fn describe_and_cache(
    artifact_path: &Path,
    bot_id: &str,
) -> Result<Vec<ConfigFieldSchema>> {
    use tokio::process::Command;
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
    let parsed: DescribeOutput = serde_json::from_str(&stdout).with_context(|| {
        format!(
            "解析 bot --describe 输出失败：{}",
            stdout.chars().take(512).collect::<String>()
        )
    })?;

    // 缓存到 schema.json。
    let schema_path = paths::bot_schema_path(bot_id);
    if let Some(parent) = schema_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 schema 缓存目录失败：{}", parent.display()))?;
    }
    let content =
        serde_json::to_string_pretty(&parsed.config_schema).context("序列化 bot schema 失败")?;
    std::fs::write(&schema_path, content)
        .with_context(|| format!("写入 schema 缓存失败：{}", schema_path.display()))?;
    Ok(parsed.config_schema)
}

/// 读取缓存的 schema（`bot --describe` 上报的结果）。
///
/// 用于表单渲染、必填校验、环境变量注入。返回 `None` 表示尚未安装制品
/// 或尚未执行 describe。
pub fn cached_schema(bot_id: &str) -> Option<Vec<ConfigFieldSchema>> {
    let path = paths::bot_schema_path(bot_id);
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FieldType;
    use std::collections::BTreeMap;

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
            id: "test".into(),
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
            id: "partial".into(),
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
            id: "t".into(),
            artifact_id: "feishu".into(),
            enabled: false,
            config,
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        let env = bot_env(&bot, &schema);
        assert!(env.is_empty());
    }
}
