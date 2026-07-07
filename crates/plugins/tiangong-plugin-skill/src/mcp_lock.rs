//! Skill ↔ MCP 依赖锁同步（mcp-lock.json）。
//!
//! 原属 `tiangong_core::app_state::repository::locks`，Skill/MCP 从 core 迁出后
//! 归入 Skill plugin——lock 的来源是 Skill 的 `requires.mcp` 声明，由 Skill plugin
//! 自行扫描 registry 聚合，写入 `~/.tiangong/skills/mcp-lock.json`。
//!
//! MCP plugin / 入口层如需参考 lock（如清理孤儿 server），由入口层协调：
//! ```ignore
//! skill_plugin.sync_mcp_dependency_lock()?;
//! ```

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::skill_registry::read_skill_manifest;

use crate::paths::{default_mcp_lock_path, default_skills_storage_dir_path};
use crate::plugin::SkillPlugin;

/// MCP 依赖锁记录：聚合所有已安装 skill 的 `requires.mcp` 声明，
/// 按 `package[@version]` 分组，记录被多少 skill 引用。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpDependencyLockRecord {
    pub ref_count: usize,
}

impl SkillPlugin {
    /// 同步 MCP 依赖锁文件（`~/.tiangong/skills/mcp-lock.json`）。
    ///
    /// 从自托管 registry 扫描已安装 skill 的 `skill.toml.requires.mcp`，
    /// 聚合依赖包名/版本，写入 mcp-lock.json。供入口层在 skill 管理操作后调用。
    pub fn sync_mcp_dependency_lock(&self) -> Result<()> {
        let skills_dir = default_skills_storage_dir_path();
        if let Err(err) = std::fs::create_dir_all(&skills_dir) {
            tracing::warn!(error = %err, "创建 skills 目录失败，跳过 mcp-lock 同步");
            return Ok(());
        }

        let registry = self.registry();
        let view = registry.view();
        let mut mcp_lock = BTreeMap::<String, McpDependencyLockRecord>::new();
        for entry in view.entries.values() {
            let manifest_path = entry.dir.join("skill.toml");
            let Ok(manifest) = read_skill_manifest(&manifest_path) else {
                continue;
            };
            for requires_mcp in &manifest.requires.mcp {
                let package = requires_mcp.package.trim();
                if package.is_empty() {
                    continue;
                }
                let version = requires_mcp.version.trim();
                let key = if version.is_empty() {
                    package.to_string()
                } else {
                    format!("{package}@{version}")
                };
                let record = mcp_lock.entry(key).or_default();
                record.ref_count += 1;
            }
        }

        let mcp_lock_path = default_mcp_lock_path();
        let mcp_content =
            serde_json::to_string_pretty(&mcp_lock).context("序列化 mcp-lock 失败")?;
        std::fs::write(&mcp_lock_path, mcp_content)
            .with_context(|| format!("写入 mcp-lock 失败：{}", mcp_lock_path.display()))?;
        Ok(())
    }
}
