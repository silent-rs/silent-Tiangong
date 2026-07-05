use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::agent_config::{InstalledSkillConfig, SkillSourceConfig};
use crate::app_state::audit;
use crate::skill::{LoadedSkill, SkillRegistryEntry, SkillRegistryView};

use super::super::*;

impl TiangongState {
    /// 返回已安装 Skill 列表（从磁盘注册表扫描，含启用与禁用）。
    ///
    /// skills 已从 AgentConfig 脱离，此处每次调用都从 registry 扫描磁盘，
    /// 不再缓存到 agent_config.skills.installed。
    pub fn installed_skills(&self) -> Vec<InstalledSkillConfig> {
        let view = self.services.skill_registry.view();
        let mut installed = Vec::new();
        for entry in view.entries.values() {
            if let Some(config) = build_installed_skill_config_from_entry(entry) {
                installed.push(config);
            }
        }
        installed.sort_by(|a, b| a.id.cmp(&b.id));
        installed
    }

    /// 返回注册表轻量视图（不含 SKILL.md 全文）
    pub fn list_skills_view(&self) -> SkillRegistryView {
        self.services.skill_registry.view()
    }

    /// 返回 Skill 完整详情（含 SKILL.md 全文），按需加载
    pub fn get_skill_detail(&self, id: &str) -> Result<Arc<LoadedSkill>> {
        self.services.skill_registry.get(id)
    }

    pub fn init_skill_scaffold(
        &self,
        path: &str,
        name: Option<&str>,
        id: Option<&str>,
        force: bool,
    ) -> Result<String> {
        let service = self.services.skill_service;
        service.init_skill_scaffold(path, name, id, force)
    }

    pub fn install_local_skill(&mut self, path: &str, enabled: bool) -> Result<String> {
        self.install_local_skill_with_options(path, enabled, false)
    }

    pub fn inspect_skill_install_requirements(
        &self,
        path: &str,
        convert_external: bool,
    ) -> Result<SkillInstallInspection> {
        let service = self.services.skill_service;
        service.inspect_skill_install_requirements(self, path, convert_external)
    }

    pub fn install_local_skill_with_options(
        &mut self,
        path: &str,
        enabled: bool,
        convert_external: bool,
    ) -> Result<String> {
        self.install_local_skill_with_options_and_inputs(path, enabled, convert_external, &[])
    }

    pub fn install_local_skill_with_options_and_inputs(
        &mut self,
        path: &str,
        enabled: bool,
        convert_external: bool,
        convert_env_values: &[(String, String)],
    ) -> Result<String> {
        let service = self.services.skill_service;
        service.install_local_skill_with_options_and_inputs(
            self,
            path,
            enabled,
            convert_external,
            convert_env_values,
        )
    }

    pub fn remove_skill(&mut self, id: &str) -> Result<String> {
        let service = self.services.skill_service;
        service.remove_skill(self, id)
    }

    pub fn set_skill_enabled(&mut self, id: &str, enabled: bool) -> Result<String> {
        let service = self.services.skill_service;
        service.set_skill_enabled(self, id, enabled)
    }

    /// 手动重扫 skills/<id>/ 注册表
    pub fn refresh_skills(&mut self) -> Result<String> {
        let view = self.services.skill_registry.refresh();
        validate_agent_config(&self.store.agent.agent_config)?;
        self.rebuild_runtime_for_agent_config();
        self.persist_agent_configs_only()?;

        let total = view.entries.len();
        let enabled = view
            .entries
            .values()
            .filter(|entry| {
                crate::skill::read_skill_manifest(&entry.dir.join("skill.toml"))
                    .map(|m| m.available)
                    .unwrap_or(false)
            })
            .count();
        Ok(format!(
            "skills 已刷新：total={total} enabled={enabled} disabled={}",
            total.saturating_sub(enabled)
        ))
    }

    /// 检测或清理手动删除 Skill 后遗留的托管 MCP 配置与锁条目。
    pub fn gc_skills(&mut self, apply: bool) -> Result<String> {
        self.services.skill_registry.refresh();
        let installed = self.installed_skills();
        let gc_report = self.build_skill_gc_report(&installed)?;

        if apply {
            if !gc_report.orphan_mcp_servers.is_empty() {
                self.store
                    .agent
                    .agent_config
                    .mcp
                    .servers
                    .retain(|server| !gc_report.orphan_mcp_servers.contains(&server.name));
            }
            validate_agent_config(&self.store.agent.agent_config)?;
            self.rebuild_runtime_for_agent_config();
            self.persist_app_only()?;
            self.persist_agent_configs_only()?;
            for backup in &gc_report.legacy_backups {
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
            format_gc_items(&gc_report.orphan_mcp_servers),
            format_gc_items(&gc_report.orphan_mcp_lock_keys),
            format_gc_paths(&gc_report.legacy_backups)
        );
        audit::append_audit_log(&audit::AuditEntry::new(
            "skill.gc",
            if apply { "apply" } else { "dry-run" },
            &message,
            true,
        ));
        Ok(message)
    }

    pub fn doctor_skills(&mut self) -> Result<String> {
        let view = self.services.skill_registry.refresh();
        let installed = self.installed_skills();
        let gc_report = self.build_skill_gc_report(&installed)?;

        let mut lines = vec![format!(
            "skill doctor: skills={} issues={} orphan_mcp_servers={} orphan_mcp_lock_entries={}",
            view.entries.len(),
            view.issues.len(),
            gc_report.orphan_mcp_servers.len(),
            gc_report.orphan_mcp_lock_keys.len()
        )];
        for issue in &view.issues {
            lines.push(format!(
                "- registry_issue kind={:?} path={} message={}",
                issue.kind,
                issue.path.display(),
                issue.message
            ));
        }
        for server in &gc_report.orphan_mcp_servers {
            lines.push(format!("- orphan_mcp_server {server}"));
        }
        for key in &gc_report.orphan_mcp_lock_keys {
            lines.push(format!("- orphan_mcp_lock_entry {key}"));
        }
        for path in &gc_report.legacy_backups {
            lines.push(format!("- stale_legacy_backup {}", path.display()));
        }
        if lines.len() == 1 {
            lines.push("- ok".to_string());
        }
        Ok(lines.join("\n"))
    }

    fn build_skill_gc_report(&self, installed: &[InstalledSkillConfig]) -> Result<SkillGcReport> {
        let declared_mcp_servers = installed
            .iter()
            .flat_map(|skill| skill.managed_mcp_servers.iter().cloned())
            .collect::<HashSet<_>>();
        let declared_lock_keys = installed
            .iter()
            .flat_map(|skill| {
                skill
                    .requires_mcp
                    .iter()
                    .filter_map(mcp_lock_key_for_requirement)
            })
            .collect::<HashSet<_>>();

        let orphan_mcp_servers = self
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .iter()
            .filter(|server| {
                server.name.starts_with("skill::") && !declared_mcp_servers.contains(&server.name)
            })
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();

        let mcp_lock_path = default_mcp_lock_path();
        let mcp_lock = read_mcp_dependency_lock_for_gc(&mcp_lock_path)
            .with_context(|| format!("读取 mcp-lock 失败：{}", mcp_lock_path.display()))?;
        let orphan_mcp_lock_keys = mcp_lock
            .keys()
            .filter(|key| !declared_lock_keys.contains(*key))
            .cloned()
            .collect::<Vec<_>>();

        Ok(SkillGcReport {
            orphan_mcp_servers,
            orphan_mcp_lock_keys,
            legacy_backups: collect_stale_legacy_backups(&default_skills_storage_dir_path()),
        })
    }
}

/// 从注册表 entry 构建 InstalledSkillConfig（轻量，只读 skill.toml，不读 SKILL.md）
fn build_installed_skill_config_from_entry(
    entry: &SkillRegistryEntry,
) -> Option<InstalledSkillConfig> {
    let manifest = crate::skill::read_skill_manifest(&entry.dir.join("skill.toml")).ok()?;
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

fn mcp_lock_key_for_requirement(requirement: &SkillMcpRequirementConfig) -> Option<String> {
    let package = requirement.package.trim();
    if package.is_empty() {
        return None;
    }
    let version = requirement.version.trim();
    if version.is_empty() {
        Some(package.to_string())
    } else {
        Some(format!("{package}@{version}"))
    }
}

fn read_mcp_dependency_lock_for_gc(
    path: &Path,
) -> Result<BTreeMap<String, McpDependencyLockRecord>> {
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

fn format_gc_paths(items: &[PathBuf]) -> String {
    if items.is_empty() {
        "-".to_string()
    } else {
        items
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn collect_stale_legacy_backups(root: &Path) -> Vec<PathBuf> {
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
    legacy_backups: Vec<PathBuf>,
}
