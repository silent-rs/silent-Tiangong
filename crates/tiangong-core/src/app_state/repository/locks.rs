use super::*;

impl AppRepository {
    pub(in crate::app_state) fn sync_skill_locks(&self, agent_config: &AgentConfig) -> Result<()> {
        let skills_dir = default_skills_storage_dir_path();
        ensure_dir(&skills_dir)?;

        let mut skills_lock = BTreeMap::<String, SkillsLockRecord>::new();
        for skill in &agent_config.skills.installed {
            let source = format!("{}:{}", skill.source.kind, skill.source.value);
            skills_lock.insert(
                skill.id.clone(),
                SkillsLockRecord {
                    version: skill.version.clone(),
                    enabled: skill.enabled,
                    source,
                    installed_at: skill.installed_at.clone(),
                    managed_mcp_servers: skill.managed_mcp_servers.clone(),
                },
            );
        }

        let mut mcp_lock = BTreeMap::<String, McpDependencyLockRecord>::new();
        for skill in &agent_config.skills.installed {
            for requires_mcp in &skill.requires_mcp {
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
                let entry = mcp_lock
                    .entry(key)
                    .or_insert_with(|| McpDependencyLockRecord {
                        path: String::new(),
                        ref_count: 0,
                        installed_at: skill.installed_at.clone(),
                    });
                entry.ref_count += 1;
            }
        }

        let skills_lock_path = default_skills_lock_path();
        let mcp_lock_path = default_mcp_lock_path();
        let skills_content =
            serde_json::to_string_pretty(&skills_lock).context("序列化 skills-lock 失败")?;
        let mcp_content =
            serde_json::to_string_pretty(&mcp_lock).context("序列化 mcp-lock 失败")?;
        fs::write(&skills_lock_path, skills_content)
            .with_context(|| format!("写入 skills-lock 失败：{}", skills_lock_path.display()))?;
        fs::write(&mcp_lock_path, mcp_content)
            .with_context(|| format!("写入 mcp-lock 失败：{}", mcp_lock_path.display()))?;
        Ok(())
    }
}
