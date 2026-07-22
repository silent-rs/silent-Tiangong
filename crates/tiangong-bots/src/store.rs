//! bot 配置持久化与审计日志。
//!
//! 采用原子写（先写 `.tmp` 再 rename）保证 bots.json 不会半写损坏，
//! 对齐 `tiangong-scheduler/src/webhook/store.rs` 与 mcp plugin 的模式。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::BotsConfig;

/// 原子写文件：先写 `<path>.tmp` 再 rename，避免半写损坏。
pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败：{}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)
        .with_context(|| format!("写入临时文件失败：{}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        format!(
            "重命名临时文件失败：{} -> {}",
            tmp.display(),
            path.display()
        )
    })?;
    Ok(())
}

/// 从路径加载 bots 配置；文件不存在或解析失败时返回默认配置（不报错）。
pub fn load_bots_config(path: &Path) -> BotsConfig {
    if !path.exists() {
        return BotsConfig::default();
    }
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|err| {
            tracing::warn!(
                "bots 配置解析失败，回退为默认配置（原文件保留）：path={} error={err}",
                path.display()
            );
            BotsConfig::default()
        }),
        Err(err) => {
            tracing::warn!(
                "读取 bots 配置失败，回退为默认配置：path={} error={err}",
                path.display()
            );
            BotsConfig::default()
        }
    }
}

/// 序列化并原子写入 bots 配置。
pub fn write_bots_config(path: &Path, config: &BotsConfig) -> Result<()> {
    let content = serde_json::to_string_pretty(config).context("序列化 bots 配置失败")?;
    atomic_write(path, &content)
}

/// 审计日志条目（对齐 `tiangong-core::observe::AuditEntry`，避免拉入 core）。
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub action: String,
    pub target: String,
    pub detail: String,
    pub success: bool,
}

impl AuditEntry {
    pub fn new(action: &str, target: &str, detail: &str, success: bool) -> Self {
        Self {
            timestamp: chrono::Local::now().naive_local().to_string(),
            action: action.to_string(),
            target: target.to_string(),
            detail: detail.to_string(),
            success,
        }
    }
}

/// 追加审计条目到 `~/.tiangong/audit.jsonl`。
///
/// 写入失败仅记录 warning，不影响调用方的主流程。
pub fn append_audit_log(entry: &AuditEntry) {
    let path = crate::paths::audit_log_path();
    append_audit_log_to(&path, entry);
}

fn append_audit_log_to(path: &Path, entry: &AuditEntry) {
    let line = match serde_json::to_string(entry) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!("审计条目序列化失败：{err}");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("创建审计日志目录失败：{} error={err}", parent.display());
        return;
    }
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(mut file) => {
            if let Err(err) = writeln!(file, "{line}") {
                tracing::warn!("写入审计日志失败：{} error={err}", path.display());
            }
        }
        Err(err) => tracing::warn!("打开审计日志失败：{} error={err}", path.display()),
    }
}

/// 审计日志路径（供测试注入临时路径）。
pub fn audit_log_path() -> PathBuf {
    crate::paths::audit_log_path()
}
