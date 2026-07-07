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

        let app_agent_config = store.agent.agent_config.clone();
        let payload = PersistedAppState {
            active_session_id: store.session.active_session_id.clone(),
            workspace_dir: store.session.workspace_dir.clone(),
            model_list: store.provider.model_list.clone(),
            agent_config: Some(app_agent_config),
        };
        let content = self.serialize_app_payload_without_mcp(&payload)?;
        fs::write(&self.paths.app_storage_path, content).with_context(|| {
            format!(
                "写入应用存储失败：{}",
                self.paths.app_storage_path.display()
            )
        })?;
        // 注意：不再调用 persist_agent_configs / sync_mcp_dependency_lock。
        // agent 配置（skills.json、mcp.json）仅由显式修改它们的操作写入，
        // 避免多进程共享数据目录时互相覆盖。
        Ok(())
    }

    pub(in crate::app_state) fn persist_to_disk(&self, store: &AppStore) -> Result<()> {
        ensure_dir(&self.paths.sessions_dir_path)?;
        ensure_parent_dir(&self.paths.app_storage_path)?;

        for session in &store.session.sessions {
            let session_path = session_storage_path(&self.paths.sessions_dir_path, &session.id);
            let content = serde_json::to_string_pretty(session)
                .with_context(|| format!("序列化会话失败：{}", session.id))?;
            fs::write(&session_path, content)
                .with_context(|| format!("写入会话文件失败：{}", session_path.display()))?;
        }

        // 不再删除不在内存中的会话文件：多进程（桌面端 / server）共享同一数据目录，
        // 每个进程只持有自己加载到内存的会话子集，删除"未知"文件会导致其他进程创建的会话丢失。
        // 会话文件的清理由 delete_active_session 显式执行。

        let app_agent_config = store.agent.agent_config.clone();
        let payload = PersistedAppState {
            active_session_id: store.session.active_session_id.clone(),
            workspace_dir: store.session.workspace_dir.clone(),
            model_list: store.provider.model_list.clone(),
            agent_config: Some(app_agent_config),
        };
        let content = self.serialize_app_payload_without_mcp(&payload)?;
        fs::write(&self.paths.app_storage_path, content).with_context(|| {
            format!(
                "写入应用存储失败：{}",
                self.paths.app_storage_path.display()
            )
        })?;
        // 注意：不再调用 persist_agent_configs / sync_mcp_dependency_lock，理由同 persist_app_only。
        Ok(())
    }

    fn serialize_app_payload_without_mcp(&self, payload: &PersistedAppState) -> Result<String> {
        let mut value = serde_json::to_value(payload).context("序列化应用存储失败")?;
        if let Some(agent_config) = value
            .get_mut("agent_config")
            .and_then(serde_json::Value::as_object_mut)
        {
            agent_config.remove("mcp");
        }
        serde_json::to_string_pretty(&value).context("序列化应用存储失败")
    }

    pub(in crate::app_state) fn remove_session_file(&self, session_id: &str) -> Result<()> {
        let session_path = session_storage_path(&self.paths.sessions_dir_path, session_id);
        if session_path.exists() {
            fs::remove_file(&session_path)
                .with_context(|| format!("删除会话文件失败：{}", session_path.display()))?;
        }
        Ok(())
    }
}
