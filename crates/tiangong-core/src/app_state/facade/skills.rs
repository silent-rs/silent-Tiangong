use std::sync::Arc;

use crate::agent_config::SkillSourceConfig;
use crate::skill::{LoadedSkill, SkillRegistryEntry, SkillRegistryView};

use super::super::*;

impl TiangongState {
    pub fn installed_skills(&self) -> &[InstalledSkillConfig] {
        &self.store.agent.agent_config.skills.installed
    }

    /// 从文件系统注册表扫描，并同步内存中的 installed[] 缓存（包含启用与禁用 Skill）
    pub(in crate::app_state) fn sync_installed_from_registry(&mut self) {
        let view = self.services.skill_registry.refresh();
        let mut installed = Vec::new();
        for entry in view.entries.values() {
            if let Some(config) =
                build_installed_skill_config_from_entry(entry, &self.services.skill_registry)
            {
                installed.push(config);
            }
        }
        installed.sort_by(|a, b| a.id.cmp(&b.id));
        self.store.agent.agent_config.skills.installed = installed;
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
}

/// 从注册表 entry 构建 InstalledSkillConfig（轻量，只读 skill.toml，不读 SKILL.md）
fn build_installed_skill_config_from_entry(
    entry: &SkillRegistryEntry,
    registry: &crate::skill::SkillRegistry,
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
    let _ = registry; // 当前不需要额外读取
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
