use super::*;

impl AppRepository {
    pub(in crate::app_state) fn persist_session(
        &self,
        store: &AppStore,
        session_id: &str,
    ) -> Result<()> {
        ensure_dir(&self.paths.sessions_dir_path)?;

        let session = store
            .session
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("会话不存在，无法持久化：{session_id}"))?;

        let session_path = session_storage_path(&self.paths.sessions_dir_path, session_id);
        let content = serde_json::to_string_pretty(session)
            .with_context(|| format!("序列化会话失败：{session_id}"))?;
        fs::write(&session_path, content)
            .with_context(|| format!("写入会话文件失败：{}", session_path.display()))
    }

    pub(in crate::app_state) fn persist_app_only(&self, store: &AppStore) -> Result<()> {
        ensure_parent_dir(&self.paths.app_storage_path)?;

        let mut app_agent_config = store.agent.agent_config.clone();
        app_agent_config.skills.installed.clear();
        let payload = PersistedAppState {
            active_session_id: store.session.active_session_id.clone(),
            workspace_dir: store.session.workspace_dir.clone(),
            model_list: store.provider.model_list.clone(),
            agent_config: Some(app_agent_config),
        };
        let content = serde_json::to_string_pretty(&payload).context("序列化应用存储失败")?;
        fs::write(&self.paths.app_storage_path, content).with_context(|| {
            format!(
                "写入应用存储失败：{}",
                self.paths.app_storage_path.display()
            )
        })?;
        self.persist_agent_configs(&store.agent.agent_config)?;
        self.sync_mcp_dependency_lock(&store.agent.agent_config)
    }

    pub(in crate::app_state) fn persist_to_disk(&self, store: &AppStore) -> Result<()> {
        ensure_dir(&self.paths.sessions_dir_path)?;
        ensure_parent_dir(&self.paths.app_storage_path)?;

        let mut session_ids_set = HashSet::new();
        for session in &store.session.sessions {
            let session_path = session_storage_path(&self.paths.sessions_dir_path, &session.id);
            let content = serde_json::to_string_pretty(session)
                .with_context(|| format!("序列化会话失败：{}", session.id))?;
            fs::write(&session_path, content)
                .with_context(|| format!("写入会话文件失败：{}", session_path.display()))?;
            session_ids_set.insert(session.id.clone());
        }

        for session_id in self.list_session_ids_from_dir()? {
            if session_ids_set.contains(&session_id) {
                continue;
            }

            let stale_path = session_storage_path(&self.paths.sessions_dir_path, &session_id);
            if stale_path.exists() {
                fs::remove_file(&stale_path)
                    .with_context(|| format!("删除废弃会话文件失败：{}", stale_path.display()))?;
            }
        }

        let mut app_agent_config = store.agent.agent_config.clone();
        app_agent_config.skills.installed.clear();
        let payload = PersistedAppState {
            active_session_id: store.session.active_session_id.clone(),
            workspace_dir: store.session.workspace_dir.clone(),
            model_list: store.provider.model_list.clone(),
            agent_config: Some(app_agent_config),
        };
        let content = serde_json::to_string_pretty(&payload).context("序列化应用存储失败")?;
        fs::write(&self.paths.app_storage_path, content).with_context(|| {
            format!(
                "写入应用存储失败：{}",
                self.paths.app_storage_path.display()
            )
        })?;
        self.persist_agent_configs(&store.agent.agent_config)?;
        self.sync_mcp_dependency_lock(&store.agent.agent_config)
    }

    pub(in crate::app_state) fn remove_session_file(&self, session_id: &str) -> Result<()> {
        let session_path = session_storage_path(&self.paths.sessions_dir_path, session_id);
        if session_path.exists() {
            fs::remove_file(&session_path)
                .with_context(|| format!("删除会话文件失败：{}", session_path.display()))?;
        }
        Ok(())
    }

    fn persist_agent_configs(&self, agent_config: &AgentConfig) -> Result<()> {
        ensure_parent_dir(&self.paths.skills_config_path)?;
        ensure_parent_dir(&self.paths.mcp_config_path)?;
        // skills.installed[] 现在由文件系统注册表（skills/<id>/）管理，不再写入 skills.json
        let skills_without_installed = SkillsConfig {
            installed: Vec::new(),
            ..agent_config.skills.clone()
        };
        let skills_content = serde_json::to_string_pretty(&skills_without_installed)
            .context("序列化 skills 配置失败")?;
        let mcp_content =
            serde_json::to_string_pretty(&agent_config.mcp).context("序列化 mcp 配置失败")?;
        fs::write(&self.paths.skills_config_path, skills_content).with_context(|| {
            format!(
                "写入 skills 配置失败：{}",
                self.paths.skills_config_path.display()
            )
        })?;
        fs::write(&self.paths.mcp_config_path, mcp_content).with_context(|| {
            format!(
                "写入 mcp 配置失败：{}",
                self.paths.mcp_config_path.display()
            )
        })?;
        Ok(())
    }
}
