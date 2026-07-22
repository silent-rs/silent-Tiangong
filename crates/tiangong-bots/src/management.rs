//! bot 管理 API——CRUD + copy-on-write + 审计。
//!
//! 对齐 `tiangong-plugin-mcp/src/management.rs` 的模式：在 snapshot 上修改 +
//! 校验，成功后**先写磁盘再 commit 内存**（memory/disk 不分叉），每次变更
//! 追加审计日志。

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::config::{
    BotConfig, BotsConfig, ConfigFieldSchema, RegisterBotRequest, UpdateBotRequest,
};
use crate::store::{AuditEntry, append_audit_log, load_bots_config, write_bots_config};

/// bot 配置管理器。
///
/// 持有 bots.json 路径，提供 CRUD 操作。内部用 `RwLock<BotsConfig>` 维护
/// 内存快照，保证读多写少场景下的并发安全。
pub struct BotStore {
    config_path: PathBuf,
    config: std::sync::RwLock<BotsConfig>,
}

impl BotStore {
    /// 用应用层注入的存储根目录（`~/.tiangong/`）构造。
    pub fn with_storage_root(root: PathBuf) -> Self {
        let config_path = root.join("bots").join("bots.json");
        Self::with_config_path(config_path)
    }

    /// 用默认路径（`~/.tiangong/bots/bots.json`）构造。
    pub fn new() -> Self {
        Self::with_config_path(crate::paths::default_bots_config_path())
    }

    /// 用显式配置路径构造（主要供测试）。
    pub fn with_config_path(config_path: PathBuf) -> Self {
        let config = load_bots_config(&config_path);
        Self {
            config_path,
            config: std::sync::RwLock::new(config),
        }
    }

    /// 配置文件路径。
    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    /// 取配置快照（clone）。
    pub fn snapshot(&self) -> BotsConfig {
        self.config
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// 列出所有 bot。
    pub fn list(&self) -> Vec<BotConfig> {
        self.snapshot().bots
    }

    /// 按 id 取单个 bot。
    pub fn get(&self, id: &str) -> Option<BotConfig> {
        self.snapshot().bots.into_iter().find(|b| b.id == id)
    }

    /// 注册新 bot。
    ///
    /// id（= 名称）作为主键，同时决定运行时目录 `~/.tiangong/bots/<id>/`。
    /// 注意：此处不做 config 字段校验——schema 由 bot 二进制运行时上报
    /// （见 [`crate::runtime`] 的 describe 协议），注册时未必已安装制品。
    /// 必填字段校验在 [`crate::runtime::BotRuntime::start`] 启动时进行。
    pub fn register(&self, request: RegisterBotRequest) -> Result<BotConfig> {
        let id = request.id.trim().to_string();
        if id.is_empty() {
            return Err(anyhow!("bot 名称不能为空"));
        }

        let mut next = self.snapshot();
        if next.bots.iter().any(|b| b.id == id) {
            return Err(anyhow!("bot 已存在：{id}"));
        }
        let now = chrono::Local::now().naive_local().to_string();
        let bot = BotConfig {
            id: id.clone(),
            artifact_id: request.artifact_id,
            enabled: request.enabled,
            config: request.config,
            created_at: now.clone(),
            updated_at: now,
        };
        next.bots.push(bot);
        self.apply(next)?;
        append_audit_log(&AuditEntry::new(
            "bots.register",
            &id,
            &format!("bot 已注册：{id}"),
            true,
        ));
        self.get(&id).context("刚注册的 bot 丢失，数据不一致")
    }

    /// 更新已有 bot（id 主键不变，就地更新 config）。
    pub fn update(&self, id: &str, request: UpdateBotRequest) -> Result<BotConfig> {
        let mut next = self.snapshot();

        let idx = next
            .bots
            .iter()
            .position(|b| b.id == id)
            .ok_or_else(|| anyhow!("bot 不存在：{id}"))?;

        let bot = &mut next.bots[idx];
        bot.config = request.config;
        bot.updated_at = chrono::Local::now().naive_local().to_string();
        self.apply(next)?;
        append_audit_log(&AuditEntry::new(
            "bots.update",
            id,
            &format!("bot 已更新：{id}"),
            true,
        ));
        self.get(id).context("刚更新的 bot 丢失，数据不一致")
    }

    /// 删除 bot。
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut next = self.snapshot();
        let prev_len = next.bots.len();
        next.bots.retain(|b| b.id != id);
        if next.bots.len() == prev_len {
            return Err(anyhow!("bot 不存在：{id}"));
        }
        self.apply(next)?;
        append_audit_log(&AuditEntry::new("bots.remove", id, "bot 已删除", true));
        Ok(())
    }

    /// 切换 bot 启用状态。
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<BotConfig> {
        let mut next = self.snapshot();
        let bot = next
            .bots
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| anyhow!("bot 不存在：{id}"))?;
        bot.enabled = enabled;
        bot.updated_at = chrono::Local::now().naive_local().to_string();
        let bot_name = bot.id.clone();
        self.apply(next)?;
        append_audit_log(&AuditEntry::new(
            "bots.toggle",
            id,
            &format!("bot {bot_name} 已{}", if enabled { "启用" } else { "禁用" }),
            true,
        ));
        self.get(id).context("刚切换的 bot 丢失，数据不一致")
    }

    /// 应用已校验通过的新配置：**先写磁盘，成功后再 commit 内存**。
    fn apply(&self, next: BotsConfig) -> Result<()> {
        write_bots_config(&self.config_path, &next)?;
        if let Ok(mut guard) = self.config.write() {
            *guard = next;
        } else {
            return Err(anyhow!("bots 配置锁中毒"));
        }
        Ok(())
    }
}

impl Default for BotStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 校验 config 是否满足给定 schema 的必填要求。
///
/// schema 来自 bot 二进制 `--describe` 上报（运行时缓存），故此函数在
/// [`crate::runtime::BotRuntime::start`] 启动时调用，而非注册时。
pub fn validate_bot_config_fields(
    schema: &[ConfigFieldSchema],
    config: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    for field in schema {
        if field.required {
            let value = config.get(&field.key);
            // 存在性 + 非空：JSON null / 空字符串 / 纯空白均视为缺失。
            let present = value
                .map(|v| {
                    if v.is_null() {
                        return false;
                    }
                    // 字符串类型额外校验 trim 后非空。
                    v.as_str().map(|s| !s.trim().is_empty()).unwrap_or(true)
                })
                .unwrap_or(false);
            if !present {
                return Err(anyhow!("缺少必填字段：{}", field.label));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, BotStore) {
        let dir = TempDir::new().unwrap();
        let store = BotStore::with_config_path(dir.path().join("bots.json"));
        (dir, store)
    }

    fn feishu_config(app_id: &str, app_secret: &str) -> BTreeMap<String, serde_json::Value> {
        let mut m = BTreeMap::new();
        m.insert("app_id".into(), serde_json::json!(app_id));
        m.insert("app_secret".into(), serde_json::json!(app_secret));
        m
    }

    #[test]
    fn register_and_get() {
        let (_dir, store) = test_store();
        let req = RegisterBotRequest {
            id: "我的飞书".into(),
            artifact_id: "feishu".into(),
            config: feishu_config("cli_x", "secret"),
            enabled: true,
        };
        let bot = store.register(req).unwrap();
        assert_eq!(bot.id, "我的飞书");
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.get(&bot.id).unwrap().id, "我的飞书");
    }

    #[test]
    fn duplicate_id_rejected() {
        let (_dir, store) = test_store();
        let req = |id: &str| RegisterBotRequest {
            id: id.into(),
            artifact_id: "feishu".into(),
            config: feishu_config("cli_x", "secret"),
            enabled: true,
        };
        store.register(req("a")).unwrap();
        let err = store.register(req("a")).unwrap_err();
        assert!(format!("{err}").contains("已存在"));
    }

    #[test]
    fn validate_required_fields() {
        use crate::config::FieldType;
        let schema = vec![ConfigFieldSchema {
            key: "app_id".into(),
            label: "App ID".into(),
            field_type: FieldType::Barcode,
            required: true,
            env: None,
            default: None,
            help: None,
        }];
        // 缺必填 → 报错
        let empty: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let err = validate_bot_config_fields(&schema, &empty).unwrap_err();
        assert!(format!("{err}").contains("必填字段"));
        // 满足必填 → 通过
        let mut ok = BTreeMap::new();
        ok.insert("app_id".into(), serde_json::json!("cli_x"));
        assert!(validate_bot_config_fields(&schema, &ok).is_ok());
        // 空字符串 → 视为缺失
        let mut blank = BTreeMap::new();
        blank.insert("app_id".into(), serde_json::json!("   "));
        let err = validate_bot_config_fields(&schema, &blank).unwrap_err();
        assert!(format!("{err}").contains("必填字段"));
    }

    #[test]
    fn update_preserves_id() {
        let (_dir, store) = test_store();
        let bot = store
            .register(RegisterBotRequest {
                id: "a".into(),
                artifact_id: "feishu".into(),
                config: feishu_config("cli_x", "secret"),
                enabled: true,
            })
            .unwrap();
        let updated = store
            .update(
                &bot.id,
                UpdateBotRequest {
                    config: feishu_config("cli_y", "secret2"),
                },
            )
            .unwrap();
        assert_eq!(updated.id, bot.id);
        assert_eq!(updated.config_string("app_id").unwrap(), "cli_y");
    }

    #[test]
    fn toggle_enabled() {
        let (_dir, store) = test_store();
        let bot = store
            .register(RegisterBotRequest {
                id: "a".into(),
                artifact_id: "feishu".into(),
                config: feishu_config("cli_x", "secret"),
                enabled: true,
            })
            .unwrap();
        assert!(store.get(&bot.id).unwrap().enabled);
        store.set_enabled(&bot.id, false).unwrap();
        assert!(!store.get(&bot.id).unwrap().enabled);
    }

    #[test]
    fn remove_deletes_bot() {
        let (_dir, store) = test_store();
        let bot = store
            .register(RegisterBotRequest {
                id: "a".into(),
                artifact_id: "feishu".into(),
                config: feishu_config("cli_x", "secret"),
                enabled: true,
            })
            .unwrap();
        store.remove(&bot.id).unwrap();
        assert!(store.get(&bot.id).is_none());
    }

    #[test]
    fn persistence_survives_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bots.json");
        {
            let store = BotStore::with_config_path(path.clone());
            store
                .register(RegisterBotRequest {
                    id: "a".into(),
                    artifact_id: "feishu".into(),
                    config: feishu_config("cli_x", "secret"),
                    enabled: true,
                })
                .unwrap();
        }
        let reloaded = BotStore::with_config_path(path);
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].id, "a");
    }
}
