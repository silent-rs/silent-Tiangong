use super::*;

impl AppRepository {
    pub(in crate::app_state) fn load_from_disk(&self) -> Result<Option<LoadedState>> {
        let session_ids = self.list_session_ids_from_dir()?;
        if !self.paths.app_storage_path.exists() {
            if session_ids.is_empty() {
                return Ok(None);
            }

            let agent_config = self.load_agent_config_with_fallback(None)?;
            let legacy_trust_mode = agent_config
                .as_ref()
                .map(|config| config.trust_mode)
                .unwrap_or_default();
            let mut sessions = Vec::new();
            for session_id in &session_ids {
                match self.load_session_from_disk(session_id, legacy_trust_mode) {
                    Ok(Some(session)) if session.parent_session_id.is_none() => {
                        sessions.push(session);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.backup_corrupted_session(session_id);
                        tracing::warn!("跳过损坏的会话文件 {session_id}: {e:#}");
                    }
                }
            }

            let active_session_id = resolve_active_session_id(&sessions, None);

            return Ok(Some(LoadedState {
                sessions,
                active_session_id,
                workspace_dir: default_workspace_dir(),
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
        let agent_config = self.load_agent_config_with_fallback(persisted.agent_config)?;
        let legacy_trust_mode = agent_config
            .as_ref()
            .map(|config| config.trust_mode)
            .unwrap_or_default();

        let mut sessions = Vec::new();
        for session_id in &session_ids {
            match self.load_session_from_disk(session_id, legacy_trust_mode) {
                Ok(Some(session)) if session.parent_session_id.is_none() => {
                    sessions.push(session);
                }
                Ok(_) => {}
                Err(e) => {
                    self.backup_corrupted_session(session_id);
                    tracing::warn!("跳过损坏的会话文件 {session_id}: {e:#}");
                }
            }
        }
        let active_session_id =
            resolve_active_session_id(&sessions, Some(&persisted.active_session_id));

        Ok(Some(LoadedState {
            sessions,
            active_session_id,
            workspace_dir: if persisted.workspace_dir.trim().is_empty() {
                default_workspace_dir()
            } else {
                persisted.workspace_dir
            },
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
            sessions: {
                let mut sessions = persisted.sessions;
                for session in &mut sessions {
                    session.migrate_legacy_content();
                }
                sessions
            },
            active_session_id: persisted.active_session_id,
            workspace_dir: default_workspace_dir(),
            model_list: persisted.model_list,
            agent_config: None,
        }))
    }

    fn load_agent_config_with_fallback(
        &self,
        legacy_agent_config: Option<AgentConfig>,
    ) -> Result<Option<AgentConfig>> {
        // AgentConfig 不再承载扩展能力配置（由各自 plugin 自管），
        // 此处仅用 custom-prompt.md 加载优先级回填 custom_system_prompt
        //（custom-prompt.md 优先，回退 legacy 旧字段）。
        let mut agent_config = legacy_agent_config;
        if let Some(config) = &mut agent_config {
            let legacy_prompt = config.custom_system_prompt.clone();
            let prompt =
                tiangong_config::io::load_custom_prompt(&legacy_prompt).unwrap_or(legacy_prompt);
            config.custom_system_prompt = prompt;
        }
        Ok(agent_config)
    }

    pub(in crate::app_state) fn load_session_from_disk(
        &self,
        session_id: &str,
        missing_trust_mode: tiangong_core::permission::TrustMode,
    ) -> Result<Option<Session>> {
        let session_path = session_storage_path(&self.paths.sessions_dir_path, session_id);
        if !session_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&session_path)
            .with_context(|| format!("读取会话文件失败：{}", session_path.display()))?;
        let missing_session_trust_mode = serde_json::from_str::<serde_json::Value>(&content)
            .ok()
            .and_then(|value| value.get("trust_mode").cloned())
            .is_none();
        let mut session: Session = serde_json::from_str(&content)
            .with_context(|| format!("解析会话文件失败：{}", session_path.display()))?;
        session.id = session_id.to_string();
        session.migrate_legacy_content();
        if missing_session_trust_mode {
            session.trust_mode = missing_trust_mode;
        }
        Ok(Some(session))
    }

    fn backup_corrupted_session(&self, session_id: &str) {
        let path = session_storage_path(&self.paths.sessions_dir_path, session_id);
        if !path.exists() {
            return;
        }
        let backup_path = path.with_extension("corrupted");
        let final_path = if backup_path.exists() {
            let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S");
            path.with_extension(format!("corrupted.{timestamp}"))
        } else {
            backup_path
        };
        if let Err(e) = fs::rename(&path, &final_path) {
            tracing::warn!("备份损坏会话文件失败 {}: {e}", final_path.display());
        }
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

fn resolve_active_session_id(sessions: &[Session], persisted_active_id: Option<&str>) -> String {
    if let Some(active_id) = persisted_active_id
        && sessions.iter().any(|session| session.id == active_id)
    {
        return active_id.to_string();
    }

    sessions
        .iter()
        .max_by(|left, right| {
            left.updated_at
                .cmp(&right.updated_at)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|session| session.id.clone())
        .unwrap_or_default()
}
