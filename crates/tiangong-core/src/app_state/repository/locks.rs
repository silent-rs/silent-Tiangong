use super::*;

impl AppRepository {
    pub(in crate::app_state) fn sync_mcp_dependency_lock(
        &self,
        agent_config: &AgentConfig,
    ) -> Result<()> {
        let skills_dir = default_skills_storage_dir_path();
        ensure_dir(&skills_dir)?;

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

        let mcp_lock_path = default_mcp_lock_path();
        let mcp_content =
            serde_json::to_string_pretty(&mcp_lock).context("序列化 mcp-lock 失败")?;
        fs::write(&mcp_lock_path, mcp_content)
            .with_context(|| format!("写入 mcp-lock 失败：{}", mcp_lock_path.display()))?;
        Ok(())
    }
}
