use std::sync::Arc;

use crate::agent_config::InstalledSkillConfig;
use crate::skill::{LoadedSkill, SkillRegistryView};

use super::super::*;

impl TiangongState {
    /// 返回已安装 Skill 列表（从磁盘注册表扫描，含启用与禁用）。
    ///
    /// skills 已从 AgentConfig 脱离，此处每次调用都从 registry 扫描磁盘。
    /// 保留为 core 的只读便捷 accessor（供 server / completion / doctor 等读取）。
    /// 写操作（remove/set_enabled/refresh/gc/doctor）已迁移至 `tiangong-plugin-skill`。
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

    /// 返回注册表轻量视图（不含 SKILL.md 全文）。
    pub fn list_skills_view(&self) -> SkillRegistryView {
        self.services.skill_registry.view()
    }

    /// 返回 Skill 完整详情（含 SKILL.md 全文），按需加载。
    pub fn get_skill_detail(&self, id: &str) -> Result<Arc<LoadedSkill>> {
        self.services.skill_registry.get(id)
    }
}

/// 从注册表 entry 构建 InstalledSkillConfig（轻量，只读 skill.toml，不读 SKILL.md）。
fn build_installed_skill_config_from_entry(
    entry: &crate::skill::SkillRegistryEntry,
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
        source: crate::agent_config::SkillSourceConfig {
            kind: "local".to_string(),
            value: entry.dir.display().to_string(),
        },
        requires_mcp: manifest.requires.mcp,
        permissions: manifest.permissions,
    })
}
