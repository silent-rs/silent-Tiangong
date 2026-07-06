//! App/CLI Skill 管理 API。
//!
//! 这些方法直接在 [`SkillPlugin`] 上实现，供 App/Tauri/CLI 入口层调用。
//! 插件自托管 [`SkillRegistry`]，管理操作直接读写磁盘，不依赖 `TiangongState`。
//!
//! 职责边界：
//! - **plugin 负责**：skill 文件系统操作（删除目录、改 skill.toml.available、扫描 registry）
//! - **入口层负责**：remove 后清理孤儿 MCP 配置 + 触发 runtime rebuild
//!   （plugin 的 remove 返回需清理的 mcp server 名列表，入口层据此操作 TiangongState）
//!
//! 安装不经专用工具——由 prompt 引导 Agent 用文件工具在 skills 目录下创建
//! `skill.toml` + `SKILL.md`。

use std::fs;
use std::sync::Arc;

use anyhow::{Context, Result};

use tiangong_core::app_state::audit;

use crate::plugin::SkillPlugin;
use crate::skill_config::{InstalledSkillConfig, SkillSourceConfig};
use crate::skill_registry::{LoadedSkill, SkillRegistryView, read_skill_manifest};

/// remove_skill 的返回值：操作消息 + 需入口层清理的孤儿 MCP server 名。
pub struct RemoveOutcome {
    pub message: String,
    /// 不再被任何已安装 skill 引用的托管 MCP server 名（`skill::<id>::<mcp_id>`）。
    /// 入口层应据此从 agent_config.mcp.servers 移除并 rebuild runtime。
    pub orphan_mcp_servers: Vec<String>,
}

impl SkillPlugin {
    /// 已安装 Skill 列表（含启用与禁用），从 registry 扫描。
    pub fn installed_skills(&self) -> Vec<InstalledSkillConfig> {
        let view = self.registry().view();
        let mut installed = Vec::new();
        for entry in view.entries.values() {
            if let Some(config) = build_installed_skill_config_from_entry(entry) {
                installed.push(config);
            }
        }
        installed.sort_by(|a, b| a.id.cmp(&b.id));
        installed
    }

    /// 注册表轻量视图（不含 SKILL.md 全文）。
    pub fn list_skills_view(&self) -> SkillRegistryView {
        self.registry().view()
    }

    /// Skill 完整详情（含 SKILL.md 全文），按需加载。
    pub fn get_skill_detail(&self, id: &str) -> Result<Arc<LoadedSkill>> {
        self.registry().get(id)
    }

    /// 卸载 Skill：删除目录 + 刷新 registry，返回需清理的孤儿 MCP server 名。
    pub fn remove_skill(&self, id: &str) -> Result<RemoveOutcome> {
        let id = id.trim();
        if id.is_empty() {
            anyhow::bail!("skill id 不能为空");
        }

        let registry = self.registry();
        let view = registry.view();
        let entry = view
            .entries
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未找到 skill：{id}"))?;
        let skill_dir = entry.dir.clone();

        // 收集该 skill 声明的托管 MCP server 名（用于判断哪些变成孤儿）。
        let removed_managed: Vec<String> = read_skill_manifest(&skill_dir.join("skill.toml"))
            .ok()
            .map(|m| {
                m.requires
                    .mcp
                    .iter()
                    .map(|req| {
                        let mcp_id = if req.id.trim().is_empty() {
                            req.package.trim()
                        } else {
                            req.id.trim()
                        };
                        format!("skill::{id}::{mcp_id}")
                    })
                    .collect()
            })
            .unwrap_or_default();

        // 删除目录
        if skill_dir.exists() {
            fs::remove_dir_all(&skill_dir)
                .with_context(|| format!("删除 skill 目录失败：{}", skill_dir.display()))?;
        }

        // 刷新 registry
        registry.invalidate(id);
        registry.refresh();

        // 找出孤儿：被删除的 skill 声明过、但删除后没有任何其他 skill 引用的 mcp server。
        let installed_after = self.installed_skills();
        let all_declared: std::collections::HashSet<String> = installed_after
            .iter()
            .flat_map(|s| s.managed_mcp_servers.iter().cloned())
            .collect();
        let orphan_mcp_servers: Vec<String> = removed_managed
            .into_iter()
            .filter(|name| !all_declared.contains(name))
            .collect();

        audit::append_audit_log(&audit::AuditEntry::new(
            "skill.remove",
            id,
            &format!("skill 已删除：{id}"),
            true,
        ));

        Ok(RemoveOutcome {
            message: format!("skill 已删除：{id}"),
            orphan_mcp_servers,
        })
    }

    /// 启用/禁用 Skill：写 skill.toml 的 available 字段。
    pub fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<String> {
        let id = id.trim();
        if id.is_empty() {
            anyhow::bail!("skill id 不能为空");
        }
        let registry = self.registry();
        registry
            .set_available(id, enabled)
            .with_context(|| format!("设置 skill available 失败：{id}"))?;

        audit::append_audit_log(&audit::AuditEntry::new(
            "skill.toggle",
            id,
            &format!("enabled={enabled}"),
            true,
        ));
        Ok(format!("skill 状态已更新：{id} enabled={enabled}"))
    }

    /// 手动重扫 skills 注册表。
    pub fn refresh_skills(&self) -> Result<String> {
        let view = self.registry().refresh();
        let total = view.entries.len();
        let enabled = view
            .entries
            .values()
            .filter(|entry| {
                read_skill_manifest(&entry.dir.join("skill.toml"))
                    .map(|m| m.available)
                    .unwrap_or(false)
            })
            .count();
        Ok(format!(
            "skills 已刷新：total={total} enabled={enabled} disabled={}",
            total.saturating_sub(enabled)
        ))
    }
}

/// 从 registry entry 构建 InstalledSkillConfig（轻量，只读 skill.toml，不读 SKILL.md）。
pub(crate) fn build_installed_skill_config_from_entry(
    entry: &crate::skill_registry::SkillRegistryEntry,
) -> Option<InstalledSkillConfig> {
    let manifest = read_skill_manifest(&entry.dir.join("skill.toml")).ok()?;
    let managed_mcp_servers = manifest
        .requires
        .mcp
        .iter()
        .map(|m| {
            if m.id.trim().is_empty() {
                format!("skill::{}::{}", entry.id, m.package)
            } else {
                format!("skill::{}::{}", entry.id, m.id)
            }
        })
        .collect::<Vec<_>>();
    Some(InstalledSkillConfig {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        description: manifest.description,
        entry: manifest.entry,
        enabled: manifest.available,
        installed_at: String::new(),
        managed_mcp_servers,
        source: SkillSourceConfig {
            kind: "local".to_string(),
            value: entry.dir.display().to_string(),
        },
        requires_mcp: manifest.requires.mcp,
        permissions: manifest.permissions,
    })
}
