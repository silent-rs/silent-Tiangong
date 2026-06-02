use crate::models_config::ModelsConfig;

use super::super::*;

impl TiangongState {
    pub fn load_or_default() -> Self {
        let app_storage_path = default_app_storage_path();
        let skills_config_path = default_skills_config_path();
        let mcp_config_path = default_mcp_config_path();
        let mcp_capability_cache_path = default_mcp_capability_cache_path();
        let sessions_dir_path = default_sessions_dir_path();
        let default_agent_config = AgentConfig::default();

        // ModelsConfig 为主配置源
        let mut models_config = ModelsConfig::load();
        if models_config.is_empty() {
            // 从环境变量生成默认 ModelsConfig
            let env_config = ModelProviderConfig::from_env();
            if !env_config.api_auth_token.is_empty() {
                models_config = ModelsConfig::from_legacy(&env_config);
                let _ = models_config.save();
            }
        }

        // 从 models_config 生成内部 ModelProviderConfig
        let model_config = models_config.to_chat_provider_config();

        let context_limit = crate::core_config::resolve_context_limit(&model_config.api_model);
        let runtime = RuntimeEngine::new(
            SingleProviderClient::new(model_config.clone()),
            context_limit,
            default_agent_config.clone(),
        )
        .with_models_config(models_config.clone());

        let mut state = Self {
            store: AppStore {
                session: SessionState {
                    sessions: Vec::new(),
                    active_session_id: String::new(),
                    workspace_dir: default_workspace_dir(),
                    session_title_draft: DEFAULT_SESSION_TITLE.to_string(),
                    input_draft: String::new(),
                },
                provider: ProviderState {
                    models_config,
                    model_config,
                    model_list: Vec::new(),
                },
                agent: AgentState {
                    agent_config: default_agent_config,
                },
                runtime: RuntimeState {
                    run: RunSnapshot::default(),
                    pending_turns: HashMap::new(),
                },
            },
            services: AppServices {
                skill_service: AppSkillService,
                mcp_service: AppMcpService,
                repository: AppRepository::new(AppPaths {
                    app_storage_path,
                    skills_config_path,
                    mcp_config_path,
                    mcp_capability_cache_path,
                    sessions_dir_path,
                }),
                runtime,
                turn_service: AppTurnService,
                skill_registry: std::sync::Arc::new(crate::skill::SkillRegistry::new(
                    default_skills_storage_dir_path(),
                )),
            },
        };

        let mut loaded_from_disk = false;
        if let Ok(Some(loaded)) = state.load_from_disk() {
            state.apply_loaded_state(loaded);
            loaded_from_disk = true;
        } else if let Ok(Some(legacy_loaded)) = state.load_from_legacy_disk() {
            state.apply_loaded_state(legacy_loaded);
            let _ = state.persist_to_disk();
            let _ = state.persist_agent_configs_only();
        }

        state.migrate_legacy_skill_layout_if_needed();

        if loaded_from_disk
            && (!state
                .services
                .repository
                .paths()
                .skills_config_path
                .exists()
                || !state.services.repository.paths().mcp_config_path.exists())
        {
            let _ = state.persist_app_only();
        }

        if state.store.session.sessions.is_empty() {
            let mut session = Session::new(DEFAULT_SESSION_TITLE);
            session.cwd = state.store.session.workspace_dir.clone();
            session.trust_mode = state.store.agent.agent_config.default_trust_mode;
            state.store.session.active_session_id = session.id.clone();
            state.store.session.sessions.push(session);
            let _ = state.persist_to_disk();
        }

        if !state
            .store
            .session
            .sessions
            .iter()
            .any(|session| session.id == state.store.session.active_session_id)
        {
            state.store.session.active_session_id = state
                .store
                .session
                .sessions
                .first()
                .map(|session| session.id.clone())
                .unwrap_or_default();
        }

        state.store.session.session_title_draft = state
            .active_session()
            .map(|session| session.title.clone())
            .unwrap_or_else(|| DEFAULT_SESSION_TITLE.to_string());
        let active_trust_mode = state.active_session_trust_mode();
        state.store.agent.agent_config.trust_mode = active_trust_mode;
        state
            .services
            .runtime
            .permission_gate()
            .set_trust_mode(active_trust_mode);
        state.rebuild_runtime_from_current_config();
        state.store.provider.model_list = normalize_model_list(
            state.store.provider.model_list.clone(),
            &state.store.provider.model_config.api_model,
        );

        let recovered_count = state.recover_interrupted_tasks();
        state.store.runtime.run.summary =
            format!("模型供应商：{}", state.services.runtime.provider_label());
        if recovered_count > 0 {
            state.store.runtime.run.status = RunStatus::Failed;
            state.store.runtime.run.summary =
                format!("已恢复 {recovered_count} 个中断任务（标记为失败）");
            state.store.runtime.run.last_result = Some("recovered_interrupted_tasks".to_string());
            state.store.runtime.run.last_error =
                Some("存在未完成任务，已在启动时恢复为失败".to_string());
            let _ = state.persist_to_disk();
        }

        if let Ok(true) = state.try_auto_resume_unfinished_plan_for_active_session() {
            state.store.runtime.run.summary =
                "检测到未完成 plan，已在启动时自动继续执行".to_string();
            state.store.runtime.run.last_result =
                Some("auto_resumed_unfinished_plan_on_startup".to_string());
            state.store.runtime.run.last_error = None;
        }
        state.store.runtime.run.updated_at = now_text();
        // EventLoop 状态恢复已移除（TiangongCore 统一管理执行状态）

        // 从文件系统注册表扫描并同步内存中的 installed[] 缓存
        state.sync_installed_from_registry();
        let _ = state.sync_mcp_dependency_lock();
        let _ = load_mcp_capabilities_cache(
            &state.services.repository.paths().mcp_capability_cache_path,
        );
        configure_mcp_capability_scheduler(
            state.store.agent.agent_config.mcp.clone(),
            state
                .services
                .repository
                .paths()
                .mcp_capability_cache_path
                .clone(),
            MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        refresh_mcp_capabilities_async(state.store.agent.agent_config.mcp.clone());

        state
    }

    fn apply_loaded_state(&mut self, loaded: LoadedState) {
        self.store.session.sessions = loaded.sessions;
        self.store.session.active_session_id = loaded.active_session_id;
        self.store.session.workspace_dir = loaded.workspace_dir;
        self.store.provider.model_list = loaded.model_list;
        if let Some(agent_config) = loaded.agent_config {
            self.store.agent.agent_config = agent_config;
            // agent_config 变更后需重建 runtime，否则 runtime 持有默认空 MCP 配置
            self.rebuild_runtime_from_current_config();
        }
    }

    pub(in crate::app_state) fn rebuild_runtime_for_agent_config(&mut self) {
        self.rebuild_runtime_from_current_config();
        configure_mcp_capability_scheduler(
            self.store.agent.agent_config.mcp.clone(),
            self.services
                .repository
                .paths()
                .mcp_capability_cache_path
                .clone(),
            MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        refresh_mcp_capabilities_async(self.store.agent.agent_config.mcp.clone());
    }

    fn migrate_legacy_skill_layout_if_needed(&mut self) {
        let fail_lock_path = default_skills_storage_dir_path().join("migration-failed.lock");
        if fail_lock_path.exists() {
            return;
        }

        let migration_result = self.try_migrate_legacy_skill_layout();
        if let Err(err) = migration_result {
            let _ = ensure_dir(&default_skills_storage_dir_path());
            let fail_content = format!(
                "time={}\nerror={}\n",
                chrono::Local::now().naive_local(),
                err
            );
            let _ = fs::write(&fail_lock_path, fail_content);
            audit::append_audit_log(&audit::AuditEntry::new(
                "skill_migration",
                "legacy_layout",
                &format!("迁移失败，已写入失败锁：{}", err),
                false,
            ));
        }
    }

    fn try_migrate_legacy_skill_layout(&mut self) -> Result<()> {
        let skills_root = default_skills_storage_dir_path();
        let legacy_installed_root = skills_root.join("installed");
        let legacy_skills_lock_path = skills_root.join("skills-lock.json");
        let legacy_skills_json_path = self.services.repository.paths().skills_config_path.clone();

        let legacy_installed = self.store.agent.agent_config.skills.installed.clone();
        let mut legacy_enabled = HashMap::<String, bool>::new();
        let mut legacy_preferred_version = HashMap::<String, String>::new();
        for item in &legacy_installed {
            legacy_enabled.insert(item.id.clone(), item.enabled);
            if !item.version.trim().is_empty() {
                legacy_preferred_version.insert(item.id.clone(), item.version.clone());
            }
        }

        let mut legacy_layout = HashMap::<String, Vec<PathBuf>>::new();
        if legacy_installed_root.exists() {
            for skill_entry in fs::read_dir(&legacy_installed_root).with_context(|| {
                format!(
                    "读取旧技能安装目录失败：{}",
                    legacy_installed_root.display()
                )
            })? {
                let skill_entry = skill_entry.with_context(|| {
                    format!(
                        "读取旧技能安装目录项失败：{}",
                        legacy_installed_root.display()
                    )
                })?;
                if !skill_entry
                    .file_type()
                    .with_context(|| {
                        format!("读取目录项类型失败：{}", skill_entry.path().display())
                    })?
                    .is_dir()
                {
                    continue;
                }
                let skill_id = skill_entry.file_name().to_string_lossy().to_string();
                if skill_id.trim().is_empty() {
                    continue;
                }

                let mut versions = Vec::new();
                for version_entry in fs::read_dir(skill_entry.path()).with_context(|| {
                    format!("读取旧技能版本目录失败：{}", skill_entry.path().display())
                })? {
                    let version_entry = version_entry.with_context(|| {
                        format!("读取旧技能版本目录项失败：{}", skill_entry.path().display())
                    })?;
                    let version_dir = version_entry.path();
                    if !version_entry
                        .file_type()
                        .with_context(|| format!("读取目录项类型失败：{}", version_dir.display()))?
                        .is_dir()
                    {
                        continue;
                    }
                    if !version_dir.join("skill.toml").exists() {
                        continue;
                    }
                    versions.push(version_dir);
                }

                if !versions.is_empty() {
                    legacy_layout.insert(skill_id, versions);
                }
            }
        }

        let has_legacy_skills_json = !legacy_installed.is_empty();
        let has_legacy_layout = !legacy_layout.is_empty();
        let has_legacy_skills_lock =
            (has_legacy_layout || has_legacy_skills_json) && legacy_skills_lock_path.exists();

        if !has_legacy_layout && !has_legacy_skills_json && !has_legacy_skills_lock {
            return Ok(());
        }

        let mut migrated_pairs = Vec::<(String, PathBuf, PathBuf)>::new();

        for (skill_id, version_dirs) in legacy_layout {
            let mut selected: Option<PathBuf> = None;
            if let Some(preferred_version) = legacy_preferred_version.get(&skill_id) {
                selected = version_dirs
                    .iter()
                    .find(|dir| {
                        dir.file_name()
                            .and_then(|name| name.to_str())
                            .map(|name| name == preferred_version)
                            .unwrap_or(false)
                    })
                    .cloned();
            }
            if selected.is_none() {
                selected = version_dirs
                    .iter()
                    .max_by(|a, b| {
                        a.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                            .cmp(
                                b.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or_default(),
                            )
                    })
                    .cloned();
            }
            let Some(source_dir) = selected else {
                continue;
            };

            let target_dir = skills_root.join(&skill_id);
            if target_dir.exists() {
                continue;
            }
            copy_dir_recursive(&source_dir, &target_dir).with_context(|| {
                format!(
                    "迁移 skill 目录失败：{} -> {}",
                    source_dir.display(),
                    target_dir.display()
                )
            })?;

            if let Some(enabled) = legacy_enabled.get(&skill_id) {
                crate::skill::write_skill_available(&target_dir, *enabled)?;
            }

            migrated_pairs.push((skill_id, source_dir.to_path_buf(), target_dir.to_path_buf()));
        }

        if legacy_skills_json_path.exists() && has_legacy_skills_json {
            let legacy_backup = with_legacy_suffix(&legacy_skills_json_path);
            if !legacy_backup.exists() {
                fs::copy(&legacy_skills_json_path, &legacy_backup).with_context(|| {
                    format!(
                        "备份旧 skills.json 失败：{} -> {}",
                        legacy_skills_json_path.display(),
                        legacy_backup.display()
                    )
                })?;
            }
        }

        if has_legacy_skills_lock {
            let legacy_lock_backup = with_legacy_suffix(&legacy_skills_lock_path);
            if !legacy_lock_backup.exists() {
                fs::copy(&legacy_skills_lock_path, &legacy_lock_backup).with_context(|| {
                    format!(
                        "备份旧 skills-lock.json 失败：{} -> {}",
                        legacy_skills_lock_path.display(),
                        legacy_lock_backup.display()
                    )
                })?;
            }
            fs::remove_file(&legacy_skills_lock_path).with_context(|| {
                format!(
                    "删除旧 skills-lock.json 失败：{}",
                    legacy_skills_lock_path.display()
                )
            })?;
        }

        if has_legacy_layout && legacy_installed_root.exists() {
            let legacy_installed_backup = with_legacy_suffix(&legacy_installed_root);
            if !legacy_installed_backup.exists() {
                fs::rename(&legacy_installed_root, &legacy_installed_backup).with_context(
                    || {
                        format!(
                            "备份旧 skill installed 目录失败：{} -> {}",
                            legacy_installed_root.display(),
                            legacy_installed_backup.display()
                        )
                    },
                )?;
            } else {
                fs::remove_dir_all(&legacy_installed_root).with_context(|| {
                    format!(
                        "删除重复旧 skill installed 目录失败：{}",
                        legacy_installed_root.display()
                    )
                })?;
            }
        }

        if has_legacy_skills_json {
            self.store.agent.agent_config.skills.installed.clear();
            self.persist_app_only()?;
        }

        if !migrated_pairs.is_empty() || has_legacy_skills_json || has_legacy_skills_lock {
            let moved = migrated_pairs
                .iter()
                .map(|(_, from, to)| format!("{} -> {}", from.display(), to.display()))
                .collect::<Vec<_>>()
                .join("; ");
            let detail = if moved.is_empty() {
                "检测到旧布局，已完成配置备份与清理".to_string()
            } else {
                format!("检测到旧布局，已迁移：{moved}")
            };
            audit::append_audit_log(&audit::AuditEntry::new(
                "skill_migration",
                "legacy_layout",
                &detail,
                true,
            ));
        }

        Ok(())
    }
}

fn with_legacy_suffix(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.legacy", path.display()))
}
