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
//! 安装统一经 agent 的 `install_skill` LLM 工具（内容式），不再支持固定路径安装。

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use tiangong_core::agent_config::{
    InstalledSkillConfig, SkillMcpRequirementConfig, SkillSourceConfig,
};
use tiangong_core::app_state::{audit, default_mcp_lock_path, default_skills_storage_dir_path};
use tiangong_core::skill::{
    read_skill_manifest, LoadedSkill, SkillRegistryEntry, SkillRegistryView,
};

use crate::plugin::SkillPlugin;

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
        let all_declared: HashSet<String> = installed_after
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

    /// 检测/报告遗留的孤儿托管 MCP server 与锁条目。
    ///
    /// 注意：此方法只**报告**孤儿，不实际清理 mcp.servers（那需要入口层操作 TiangongState）。
    /// 入口层拿到报告后自行决定是否清理。
    pub fn gc_skills(&self, apply: bool) -> Result<String> {
        self.registry().refresh();
        let installed = self.installed_skills();
        let report = self.build_gc_report(&installed)?;

        // 清理磁盘上的遗留备份（plugin 可直接操作）
        if apply {
            for backup in &report.legacy_backups {
                if backup.is_dir() {
                    let _ = fs::remove_dir_all(backup);
                } else {
                    let _ = fs::remove_file(backup);
                }
            }
        }

        let mode = if apply { "已清理" } else { "dry-run" };
        let message = format!(
            "skill gc {mode}: orphan_mcp_servers={} orphan_mcp_lock_entries={} legacy_backups={}",
            format_gc_items(&report.orphan_mcp_servers),
            format_gc_items(&report.orphan_mcp_lock_keys),
            format_gc_paths(&report.legacy_backups)
        );
        audit::append_audit_log(&audit::AuditEntry::new(
            "skill.gc",
            if apply { "apply" } else { "dry-run" },
            &message,
            true,
        ));
        Ok(message)
    }

    /// 诊断报告。
    pub fn doctor_skills(&self) -> Result<String> {
        let view = self.registry().refresh();
        let installed = self.installed_skills();
        let report = self.build_gc_report(&installed)?;

        let mut lines = vec![format!(
            "skill doctor: skills={} issues={} orphan_mcp_servers={} orphan_mcp_lock_entries={}",
            view.entries.len(),
            view.issues.len(),
            report.orphan_mcp_servers.len(),
            report.orphan_mcp_lock_keys.len()
        )];
        for issue in &view.issues {
            lines.push(format!(
                "- registry_issue kind={:?} path={} message={}",
                issue.kind,
                issue.path.display(),
                issue.message
            ));
        }
        for server in &report.orphan_mcp_servers {
            lines.push(format!("- orphan_mcp_server {server}"));
        }
        for key in &report.orphan_mcp_lock_keys {
            lines.push(format!("- orphan_mcp_lock_entry {key}"));
        }
        for path in &report.legacy_backups {
            lines.push(format!("- stale_legacy_backup {}", path.display()));
        }
        if lines.len() == 1 {
            lines.push("- ok".to_string());
        }
        Ok(lines.join("\n"))
    }

    /// 构建 GC 报告。
    fn build_gc_report(&self, installed: &[InstalledSkillConfig]) -> Result<SkillGcReport> {
        let declared_mcp_servers: HashSet<String> = installed
            .iter()
            .flat_map(|s| s.managed_mcp_servers.iter().cloned())
            .collect();
        let declared_lock_keys: HashSet<String> = installed
            .iter()
            .flat_map(|s| {
                s.requires_mcp
                    .iter()
                    .filter_map(mcp_lock_key_for_requirement)
            })
            .collect();

        // 从缓存的 mcp servers 快照检测孤儿（skill:: 前缀但无 skill 声明）。
        let orphan_mcp_servers: Vec<String> = self
            .mcp_servers()
            .iter()
            .filter(|server| {
                server.name.starts_with("skill::") && !declared_mcp_servers.contains(&server.name)
            })
            .map(|server| server.name.clone())
            .collect();

        let mcp_lock_path = default_mcp_lock_path();
        let mcp_lock = read_mcp_dependency_lock_for_gc(&mcp_lock_path)
            .with_context(|| format!("读取 mcp-lock 失败：{}", mcp_lock_path.display()))?;
        let orphan_mcp_lock_keys: Vec<String> = mcp_lock
            .keys()
            .filter(|key| !declared_lock_keys.contains(*key))
            .cloned()
            .collect();

        Ok(SkillGcReport {
            orphan_mcp_servers,
            orphan_mcp_lock_keys,
            legacy_backups: collect_stale_legacy_backups(&default_skills_storage_dir_path()),
        })
    }
}

// ── 辅助函数 ──────────────────────────────────────────

/// 从 registry entry 构建 InstalledSkillConfig（轻量，只读 skill.toml）。
pub(crate) fn build_installed_skill_config_from_entry(
    entry: &SkillRegistryEntry,
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

fn mcp_lock_key_for_requirement(req: &SkillMcpRequirementConfig) -> Option<String> {
    let package = req.package.trim();
    if package.is_empty() {
        return None;
    }
    let version = req.version.trim();
    if version.is_empty() {
        Some(package.to_string())
    } else {
        Some(format!("{package}@{version}"))
    }
}

fn read_mcp_dependency_lock_for_gc(path: &Path) -> Result<BTreeMap<String, serde_json::Value>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(&raw).context("解析 mcp-lock 失败")
}

fn format_gc_items(items: &[String]) -> String {
    if items.is_empty() {
        "-".to_string()
    } else {
        items.join(",")
    }
}

fn format_gc_paths(items: &[std::path::PathBuf]) -> String {
    if items.is_empty() {
        "-".to_string()
    } else {
        items
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn collect_stale_legacy_backups(root: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(30 * 24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".legacy"))
        })
        .filter(|path| {
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .map(|modified| modified < cutoff)
                .unwrap_or(false)
        })
        .collect()
}

struct SkillGcReport {
    orphan_mcp_servers: Vec<String>,
    orphan_mcp_lock_keys: Vec<String>,
    legacy_backups: Vec<std::path::PathBuf>,
}
