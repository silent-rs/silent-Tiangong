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

        let mut app_agent_config = store.agent.agent_config.clone();
        app_agent_config.skills.installed.clear();
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

    /// 持久化 agent 配置（skills.json、mcp.json），写入前合并磁盘上的外部变更。
    ///
    /// 多进程（桌面端 / server）共享数据目录时，写入前先读取磁盘文件，
    /// 将其他进程的变更合并进来，避免覆盖丢失。
    /// 检测到外部变更时会记录 warn 日志。
    pub(in crate::app_state) fn persist_agent_configs(
        &self,
        agent_config: &AgentConfig,
    ) -> Result<()> {
        ensure_parent_dir(&self.paths.skills_config_path)?;
        ensure_parent_dir(&self.paths.mcp_config_path)?;

        // --- skills.json：合并磁盘上其他进程新增的 dirs ---
        let merged_skills = self.merge_skills_with_disk(&agent_config.skills)?;
        let skills_content = serde_json::to_string_pretty(&SkillsConfig {
            installed: Vec::new(),
            ..merged_skills
        })
        .context("序列化 skills 配置失败")?;
        fs::write(&self.paths.skills_config_path, skills_content).with_context(|| {
            format!(
                "写入 skills 配置失败：{}",
                self.paths.skills_config_path.display()
            )
        })?;

        // --- mcp.json：合并磁盘上其他进程新增的 server ---
        let merged_mcp = self.merge_mcp_with_disk(&agent_config.mcp)?;
        let mcp_content =
            serde_json::to_string_pretty(&merged_mcp).context("序列化 mcp 配置失败")?;
        fs::write(&self.paths.mcp_config_path, mcp_content).with_context(|| {
            format!(
                "写入 mcp 配置失败：{}",
                self.paths.mcp_config_path.display()
            )
        })?;

        Ok(())
    }

    /// 读取磁盘上的 skills.json，合并其他进程新增的 dirs。
    /// 合并策略：标量字段以内存为准，dirs 取并集。
    fn merge_skills_with_disk(&self, memory_skills: &SkillsConfig) -> Result<SkillsConfig> {
        if !self.paths.skills_config_path.exists() {
            return Ok(memory_skills.clone());
        }

        let disk_content =
            fs::read_to_string(&self.paths.skills_config_path).with_context(|| {
                format!(
                    "读取 skills 配置失败：{}",
                    self.paths.skills_config_path.display()
                )
            })?;
        let disk_skills: SkillsConfig = serde_json::from_str(&disk_content).with_context(|| {
            format!(
                "解析 skills 配置失败：{}",
                self.paths.skills_config_path.display()
            )
        })?;

        let mut merged_dirs: Vec<String> = memory_skills.dirs.clone();
        for dir in &disk_skills.dirs {
            if !merged_dirs.contains(dir) {
                merged_dirs.push(dir.clone());
            }
        }

        if disk_skills.enabled != memory_skills.enabled
            || disk_skills.max_matches != memory_skills.max_matches
            || merged_dirs.len() != memory_skills.dirs.len()
        {
            tracing::warn!(
                "检测到 skills.json 被其他进程修改，已合并外部变更（dirs 并集，标量字段以当前进程为准）"
            );
        }

        Ok(SkillsConfig {
            enabled: memory_skills.enabled,
            max_matches: memory_skills.max_matches,
            dirs: merged_dirs,
            installed: Vec::new(),
        })
    }

    /// 读取磁盘上的 mcp.json，合并其他进程新增的 server。
    /// 合并策略：同名 server 以内存（当前进程）为准，磁盘上独有的 server 保留。
    fn merge_mcp_with_disk(&self, memory_mcp: &McpConfig) -> Result<McpConfig> {
        if !self.paths.mcp_config_path.exists() {
            return Ok(memory_mcp.clone());
        }

        let disk_content = fs::read_to_string(&self.paths.mcp_config_path).with_context(|| {
            format!(
                "读取 mcp 配置失败：{}",
                self.paths.mcp_config_path.display()
            )
        })?;
        let disk_mcp: McpConfig = serde_json::from_str(&disk_content).with_context(|| {
            format!(
                "解析 mcp 配置失败：{}",
                self.paths.mcp_config_path.display()
            )
        })?;

        let memory_names: Vec<&str> = memory_mcp.servers.iter().map(|s| s.name.as_str()).collect();
        let mut external_added = Vec::new();

        let mut merged_servers = memory_mcp.servers.clone();
        for disk_server in &disk_mcp.servers {
            if !memory_names.contains(&disk_server.name.as_str()) {
                merged_servers.push(disk_server.clone());
                external_added.push(disk_server.name.clone());
            }
        }

        if !external_added.is_empty() {
            tracing::warn!(
                "检测到 mcp.json 被其他进程修改，已合并外部新增的 server：{}",
                external_added.join(", ")
            );
        }

        Ok(McpConfig {
            enabled: memory_mcp.enabled,
            timeout_ms: memory_mcp.timeout_ms,
            servers: merged_servers,
        })
    }
}
