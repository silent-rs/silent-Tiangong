use super::*;

impl AppRepository {
    pub(in crate::app_state) fn load_from_disk(&self) -> Result<Option<LoadedState>> {
        let session_ids = self.list_session_ids_from_dir()?;
        if !self.paths.app_storage_path.exists() {
            if session_ids.is_empty() {
                return Ok(None);
            }

            let agent_config = self.load_agent_config_with_fallback(None)?;
            let mut sessions = Vec::new();
            for session_id in &session_ids {
                if let Some(session) = self.load_session_from_disk(session_id)? {
                    sessions.push(session);
                }
            }

            return Ok(Some(LoadedState {
                sessions,
                active_session_id: session_ids.first().cloned().unwrap_or_default(),
                model_config: None,
                model_list: Vec::new(),
                agent_config,
            }));
        }

        let content = fs::read_to_string(&self.paths.app_storage_path).with_context(|| {
            format!(
                "读取应用存储失败：{}",
                self.paths.app_storage_path.display()
            )
        })?;
        let persisted: PersistedAppState =
            serde_json::from_str(&content).context("解析应用存储失败")?;

        let mut sessions = Vec::new();
        for session_id in &session_ids {
            if let Some(session) = self.load_session_from_disk(session_id)? {
                sessions.push(session);
            }
        }
        let active_session_id = if session_ids
            .iter()
            .any(|session_id| session_id == &persisted.active_session_id)
        {
            persisted.active_session_id
        } else {
            session_ids.first().cloned().unwrap_or_default()
        };
        let agent_config = self.load_agent_config_with_fallback(persisted.agent_config)?;

        Ok(Some(LoadedState {
            sessions,
            active_session_id,
            model_config: persisted.model_config,
            model_list: persisted.model_list,
            agent_config,
        }))
    }

    pub(in crate::app_state) fn load_from_legacy_disk(&self) -> Result<Option<LoadedState>> {
        let legacy_storage_path = default_legacy_storage_path();
        if !legacy_storage_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&legacy_storage_path)
            .with_context(|| format!("读取旧会话存储失败：{}", legacy_storage_path.display()))?;
        let persisted: LegacyPersistedState =
            serde_json::from_str(&content).context("解析旧会话存储失败")?;

        Ok(Some(LoadedState {
            sessions: persisted.sessions,
            active_session_id: persisted.active_session_id,
            model_config: persisted.model_config,
            model_list: persisted.model_list,
            agent_config: None,
        }))
    }

    fn load_agent_config_with_fallback(
        &self,
        legacy_agent_config: Option<AgentConfig>,
    ) -> Result<Option<AgentConfig>> {
        let skills = self.load_skills_config_from_disk()?;
        let mcp = self.load_mcp_config_from_disk()?;
        if skills.is_none() && mcp.is_none() {
            return Ok(legacy_agent_config);
        }
        let mut agent_config = legacy_agent_config.unwrap_or_default();
        if let Some(skills) = skills {
            agent_config.skills = skills;
        }
        if let Some(mcp) = mcp {
            agent_config.mcp = mcp;
        }
        Ok(Some(agent_config))
    }

    fn load_skills_config_from_disk(&self) -> Result<Option<SkillsConfig>> {
        if !self.paths.skills_config_path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&self.paths.skills_config_path).with_context(|| {
            format!(
                "读取 skills 配置失败：{}",
                self.paths.skills_config_path.display()
            )
        })?;
        let config: SkillsConfig = serde_json::from_str(&content).with_context(|| {
            format!(
                "解析 skills 配置失败：{}",
                self.paths.skills_config_path.display()
            )
        })?;
        Ok(Some(config))
    }

    fn load_mcp_config_from_disk(&self) -> Result<Option<McpConfig>> {
        if !self.paths.mcp_config_path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&self.paths.mcp_config_path).with_context(|| {
            format!(
                "读取 mcp 配置失败：{}",
                self.paths.mcp_config_path.display()
            )
        })?;
        let config: McpConfig = serde_json::from_str(&content).with_context(|| {
            format!(
                "解析 mcp 配置失败：{}",
                self.paths.mcp_config_path.display()
            )
        })?;
        Ok(Some(config))
    }

    pub(in crate::app_state) fn load_session_from_disk(
        &self,
        session_id: &str,
    ) -> Result<Option<Session>> {
        let session_path = session_storage_path(&self.paths.sessions_dir_path, session_id);
        if !session_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&session_path)
            .with_context(|| format!("读取会话文件失败：{}", session_path.display()))?;
        let mut session: Session = serde_json::from_str(&content)
            .with_context(|| format!("解析会话文件失败：{}", session_path.display()))?;
        session.id = session_id.to_string();
        Ok(Some(session))
    }

    pub(in crate::app_state) fn list_session_ids_from_dir(&self) -> Result<Vec<String>> {
        if !self.paths.sessions_dir_path.exists() {
            return Ok(Vec::new());
        }

        let mut session_ids = Vec::new();
        for entry in fs::read_dir(&self.paths.sessions_dir_path).with_context(|| {
            format!(
                "读取会话目录失败：{}",
                self.paths.sessions_dir_path.display()
            )
        })? {
            let entry = entry.with_context(|| {
                format!(
                    "读取会话目录项失败：{}",
                    self.paths.sessions_dir_path.display()
                )
            })?;

            let file_type = entry.file_type().with_context(|| {
                format!(
                    "读取会话目录项类型失败：{}",
                    self.paths.sessions_dir_path.display()
                )
            })?;
            if !file_type.is_file() {
                continue;
            }

            let path = entry.path();
            if path.extension() != Some(OsStr::new("json")) {
                continue;
            }

            let Some(file_stem) = path.file_stem().and_then(|v| v.to_str()) else {
                continue;
            };

            if let Some(session_id) = canonical_scru128_id(file_stem) {
                session_ids.push(session_id);
            }
        }

        session_ids.sort();
        session_ids.dedup();
        Ok(session_ids)
    }
}
