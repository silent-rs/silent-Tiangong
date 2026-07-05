use super::*;

use crate::skill::{read_skill_manifest, scan_skill_registry};

impl AppRepository {
    /// 同步 MCP 依赖锁文件（mcp-lock.json）。
    ///
    /// 从磁盘扫描已安装 skill 的 `skill.toml.requires.mcp`，聚合依赖包名/版本，
    /// 写入 `~/.tiangong/skills/mcp-lock.json`。
    ///
    /// skills 已从 AgentConfig 脱离，此处直接扫盘而非读 agent_config.skills.installed。
    pub(in crate::app_state) fn sync_mcp_dependency_lock(&self) -> Result<()> {
        let skills_dir = default_skills_storage_dir_path();
        ensure_dir(&skills_dir)?;

        let mut mcp_lock = BTreeMap::<String, McpDependencyLockRecord>::new();
        let view = scan_skill_registry(&skills_dir);
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
                let record = mcp_lock
                    .entry(key)
                    .or_insert_with(|| McpDependencyLockRecord {
                        path: String::new(),
                        ref_count: 0,
                        installed_at: String::new(),
                    });
                record.ref_count += 1;
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
