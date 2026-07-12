use super::*;
use tiangong_core::session::atomic_replace_file;

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
        atomic_replace_file(&session_path, content.as_bytes())
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
            input_drafts: persisted_input_drafts(store),
            input_draft: String::new(),
        };
        let content = self.serialize_app_payload_stripped_external_configs(&payload)?;
        atomic_replace_file(&self.paths.app_storage_path, content.as_bytes()).with_context(
            || {
                format!(
                    "写入应用存储失败：{}",
                    self.paths.app_storage_path.display()
                )
            },
        )?;
        // 注意：扩展能力配置仅由显式修改它们的插件写入，
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
            atomic_replace_file(&session_path, content.as_bytes())
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
            input_drafts: persisted_input_drafts(store),
            input_draft: String::new(),
        };
        let content = self.serialize_app_payload_stripped_external_configs(&payload)?;
        atomic_replace_file(&self.paths.app_storage_path, content.as_bytes()).with_context(
            || {
                format!(
                    "写入应用存储失败：{}",
                    self.paths.app_storage_path.display()
                )
            },
        )?;
        // 注意：扩展能力配置由各自插件写入，理由同 persist_app_only。
        Ok(())
    }

    fn serialize_app_payload_stripped_external_configs(
        &self,
        payload: &PersistedAppState,
    ) -> Result<String> {
        let mut value = serde_json::to_value(payload).context("序列化应用存储失败")?;
        if let Some(agent_config) = value
            .get_mut("agent_config")
            .and_then(serde_json::Value::as_object_mut)
        {
            // 剥离遗留的扩展能力字段（"mcp" 等磁盘格式契约字段名）——旧版 app.json
            // 可能写入过这些字段，加载/回写时统一剔除，保证 core 持久化纯净。
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

fn persisted_input_drafts(store: &AppStore) -> HashMap<String, SessionInputDraft> {
    store
        .session
        .input_drafts
        .iter()
        .map(|(session_id, draft)| {
            let mut persisted = draft.clone();
            // 进程中断后发送事务已不存在，重启必须允许用户重试。
            persisted.is_sending = false;
            (session_id.clone(), persisted)
        })
        .collect()
}
