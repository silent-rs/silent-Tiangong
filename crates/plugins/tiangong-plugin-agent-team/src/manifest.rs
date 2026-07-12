//! 父 Session 下独立子 Core 的稳定清单。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::{AgentDescriptor, AgentStatus};

const MANIFEST_VERSION: u32 = 1;
const RESERVED_ROLES: &[&str] = &["main", "all", "user", "system", "assistant", "tool"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentRecord {
    pub descriptor: AgentDescriptor,
    /// 同级等待边只允许由较小序号指向较大序号，保证调用图无环。
    pub topology_order: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TeamManifest {
    version: u32,
    pub parent_session_id: String,
    next_topology_order: u64,
    agents: Vec<AgentRecord>,
}

impl TeamManifest {
    pub fn empty(parent_session_id: impl Into<String>) -> Self {
        Self {
            version: MANIFEST_VERSION,
            parent_session_id: parent_session_id.into(),
            next_topology_order: 1,
            agents: Vec::new(),
        }
    }

    pub fn load(root: &Path, parent_session_id: &str) -> Result<Self, String> {
        let path = root.join("manifest.json");
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty(parent_session_id));
            }
            Err(error) => {
                return Err(format!(
                    "检查 Agent Team 清单失败（{}）：{error}",
                    path.display()
                ));
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("Agent Team 清单不得是符号链接：{}", path.display()));
            }
            Ok(_) => {}
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("读取 Agent Team 清单失败（{}）：{error}", path.display()))?;
        let mut manifest: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("解析 Agent Team 清单失败（{}）：{error}", path.display()))?;
        if manifest.version != MANIFEST_VERSION {
            return Err(format!(
                "不支持的 Agent Team 清单版本：{}（期望 {MANIFEST_VERSION}）",
                manifest.version
            ));
        }
        if manifest.parent_session_id != parent_session_id {
            return Err(format!(
                "Agent Team 清单父会话不匹配：期望 {parent_session_id}，实际 {}",
                manifest.parent_session_id
            ));
        }
        let mut alive_roles = std::collections::HashSet::new();
        for record in &mut manifest.agents {
            let role = validate_role_identifier(&record.descriptor.role).map_err(|error| {
                format!(
                    "Agent Team 清单中的角色 '{}' 无效：{error}",
                    record.descriptor.role
                )
            })?;
            record.descriptor.role = role.clone();
            if record.descriptor.status != AgentStatus::Terminated && !alive_roles.insert(role) {
                return Err("Agent Team 清单包含大小写冲突的存活角色".to_string());
            }
        }
        Ok(manifest)
    }

    pub fn persist(&self, root: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("序列化 Agent Team 清单失败：{error}"))?;
        tiangong_core::session::atomic_replace_file(&root.join("manifest.json"), &bytes)
            .map_err(|error| format!("持久化 Agent Team 清单失败：{error}"))
    }

    pub fn allocate_order(&mut self) -> u64 {
        let order = self.next_topology_order;
        self.next_topology_order = self.next_topology_order.saturating_add(1);
        order
    }

    pub fn upsert(&mut self, record: AgentRecord) {
        if let Some(existing) = self
            .agents
            .iter_mut()
            .find(|existing| existing.descriptor.agent_id == record.descriptor.agent_id)
        {
            *existing = record;
        } else {
            self.agents.push(record);
        }
    }

    pub fn mark_terminated(&mut self, agent_id: &str) -> bool {
        let Some(record) = self
            .agents
            .iter_mut()
            .find(|record| record.descriptor.agent_id == agent_id)
        else {
            return false;
        };
        record.descriptor.status = AgentStatus::Terminated;
        true
    }

    pub fn alive(&self) -> impl Iterator<Item = &AgentRecord> {
        self.agents
            .iter()
            .filter(|record| record.descriptor.status != AgentStatus::Terminated)
    }

    pub fn record(&self, agent_id: &str) -> Option<&AgentRecord> {
        self.agents
            .iter()
            .find(|record| record.descriptor.agent_id == agent_id)
    }

    pub fn find_by_role(&self, role: &str) -> Option<&AgentRecord> {
        let role = normalize_role(role);
        self.alive()
            .find(|record| normalize_role(&record.descriptor.role) == role)
    }

    pub fn alive_count(&self) -> usize {
        self.alive().count()
    }
}

pub(crate) fn team_root(storage_root: &Path, parent_session_id: &str) -> PathBuf {
    storage_root.join("teams").join(parent_session_id)
}

pub(crate) fn child_root(storage_root: &Path, parent_session_id: &str, agent_id: &str) -> PathBuf {
    team_root(storage_root, parent_session_id).join(agent_id)
}

pub(crate) fn normalize_role(role: &str) -> String {
    let role = role.trim();
    role.strip_prefix('@').unwrap_or(role).to_lowercase()
}

pub(crate) fn validate_role_identifier(role: &str) -> Result<String, String> {
    if role.is_empty() {
        return Err("不能为空".to_string());
    }
    if role.chars().any(char::is_whitespace) {
        return Err("不能包含空白字符".to_string());
    }
    let role = role.strip_prefix('@').unwrap_or(role);
    if role.is_empty() || role.starts_with('@') {
        return Err("必须包含有效角色名，且最多只能有一个 @ 前缀".to_string());
    }
    if !role
        .chars()
        .all(|character| character.is_alphanumeric() || character == '_')
        || !role.chars().any(char::is_alphanumeric)
    {
        return Err("只能包含字母、数字和下划线".to_string());
    }
    let role = role.to_lowercase();
    if RESERVED_ROLES.contains(&role.as_str()) {
        return Err(format!("'{role}' 是系统保留标识"));
    }
    Ok(role)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(agent_id: &str, role: &str, order: u64) -> AgentRecord {
        AgentRecord {
            descriptor: AgentDescriptor {
                agent_id: agent_id.to_string(),
                role: role.to_string(),
                label: role.to_string(),
                system_prompt: "work".to_string(),
                status: AgentStatus::Idle,
            },
            topology_order: order,
        }
    }

    #[test]
    fn manifest_round_trip_preserves_alive_view() {
        let dir = tempfile::tempdir().unwrap();
        let mut manifest = TeamManifest::empty("parent");
        let first = manifest.allocate_order();
        manifest.upsert(record("agent-dev", "dev", first));
        let second = manifest.allocate_order();
        manifest.upsert(record("agent-test", "test", second));
        assert!(manifest.mark_terminated("agent-dev"));
        manifest.persist(dir.path()).unwrap();

        let restored = TeamManifest::load(dir.path(), "parent").unwrap();
        assert_eq!(restored.alive_count(), 1);
        assert_eq!(
            restored.find_by_role("TEST").unwrap().descriptor.agent_id,
            "agent-test"
        );
    }

    #[test]
    fn roles_are_mentionable_reserved_safe_and_case_normalized() {
        assert_eq!(validate_role_identifier("@Dev_2").unwrap(), "dev_2");
        for invalid in [
            "main",
            "ALL",
            "user",
            " two",
            "two words",
            "two-words",
            "@@dev",
            "___",
        ] {
            assert!(validate_role_identifier(invalid).is_err(), "{invalid}");
        }
    }
}
