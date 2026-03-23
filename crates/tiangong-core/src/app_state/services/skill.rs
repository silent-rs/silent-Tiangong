use super::super::*;

#[derive(Debug, Clone, Copy, Default)]
pub struct AppSkillService;

impl AppSkillService {
    pub(in crate::app_state) fn init_skill_scaffold(
        self,
        path: &str,
        name: Option<&str>,
        id: Option<&str>,
        force: bool,
    ) -> Result<String> {
        let path = path.trim();
        if path.is_empty() {
            return Err(anyhow!("skill 初始化目录不能为空"));
        }
        let result = init_tiangong_skill_scaffold(Path::new(path), name, id, force)?;
        Ok(format!(
            "skill 初始化完成：id={} name={} path={}",
            result.skill_id,
            result.skill_name,
            result.dir.display()
        ))
    }

    pub(in crate::app_state) fn inspect_skill_install_requirements(
        self,
        state: &TiangongState,
        path: &str,
        convert_external: bool,
    ) -> Result<SkillInstallInspection> {
        let source_path = self.resolve_skill_source_path(state, path)?;
        if !convert_external {
            return Ok(SkillInstallInspection::default());
        }
        let analysis = analyze_external_skill(&source_path)?;
        let missing_env_vars = analysis
            .env_vars
            .iter()
            .filter(|key| std::env::var_os(key.as_str()).is_none())
            .cloned()
            .collect::<Vec<_>>();
        Ok(SkillInstallInspection {
            dependencies: analysis.dependencies,
            env_vars: analysis.env_vars,
            missing_env_vars,
        })
    }

    pub(in crate::app_state) fn install_local_skill_with_options_and_inputs(
        self,
        state: &mut TiangongState,
        path: &str,
        enabled: bool,
        convert_external: bool,
        convert_env_values: &[(String, String)],
    ) -> Result<String> {
        let path = path.trim();
        if path.is_empty() {
            return Err(anyhow!("skill 路径不能为空"));
        }
        let source_path = self.resolve_skill_source_path(state, path)?;

        let mut pre_conversion_notes = Vec::new();
        let llm_artifacts = if convert_external {
            let need_skill_md = !source_path.join("SKILL.md").exists();
            let need_skill_toml = !source_path.join("skill.toml").exists();
            if need_skill_md || need_skill_toml {
                let model_client =
                    SingleProviderClient::new(state.store.provider.model_config.clone());
                match convert_external_skill_with_agent(
                    &model_client,
                    &source_path,
                    need_skill_md,
                    need_skill_toml,
                ) {
                    Ok(artifacts) => {
                        pre_conversion_notes.push("已调用模型辅助转换".to_string());
                        Some(artifacts)
                    }
                    Err(err) => {
                        pre_conversion_notes
                            .push(format!("模型辅助转换失败，已回退规则转换：{err}"));
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut prepared_source = prepare_skill_source_for_install(
            &source_path,
            convert_external,
            llm_artifacts.as_ref(),
            convert_env_values,
        )?;
        let stage_cleanup = ScopedDirCleanup::new(converted_stage_cleanup_dir(
            &prepared_source.install_path,
            prepared_source.converted,
        ));
        let stage_cleanup_enabled = stage_cleanup.is_enabled();
        if !pre_conversion_notes.is_empty() {
            let mut merged_notes = pre_conversion_notes;
            merged_notes.extend(prepared_source.conversion_notes);
            prepared_source.conversion_notes = merged_notes;
        }
        let mut skill = load_skill_from_local_dir(&prepared_source.install_path)?;

        if state
            .store
            .agent
            .agent_config
            .skills
            .installed
            .iter()
            .any(|item| item.id == skill.id)
        {
            return Err(anyhow!("skill 已存在：{}", skill.id));
        }

        let installed_dir = default_skills_storage_dir_path()
            .join("installed")
            .join(&skill.id)
            .join(&skill.version);
        if installed_dir.exists() {
            return Err(anyhow!(
                "skill 已存在同版本目录：{}",
                installed_dir.display()
            ));
        }
        if let Some(parent) = installed_dir.parent() {
            ensure_dir(parent)?;
        }
        copy_dir_recursive(&prepared_source.install_path, &installed_dir).with_context(|| {
            format!(
                "复制 skill 到安装目录失败：{} -> {}",
                prepared_source.install_path.display(),
                installed_dir.display()
            )
        })?;

        skill.source.kind = "local".to_string();
        skill.source.value = installed_dir.display().to_string();
        skill.enabled = enabled;

        state
            .store
            .agent
            .agent_config
            .skills
            .installed
            .push(skill.clone());
        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        state.sync_skill_locks()?;
        let mut message = format!(
            "skill 已安装：{}@{} enabled={}",
            skill.id, skill.version, skill.enabled
        );
        if prepared_source.converted {
            if stage_cleanup_enabled {
                prepared_source
                    .conversion_notes
                    .push("转换中间目录已自动清理".to_string());
            }
            let details = if prepared_source.conversion_notes.is_empty() {
                "已执行外部 skill 转换".to_string()
            } else {
                prepared_source.conversion_notes.join("，")
            };
            message.push_str(&format!("（{details}）"));
        }
        Ok(message)
    }

    pub(in crate::app_state) fn remove_skill(
        self,
        state: &mut TiangongState,
        id: &str,
    ) -> Result<String> {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow!("skill id 不能为空"));
        }
        let Some(remove_idx) = state
            .store
            .agent
            .agent_config
            .skills
            .installed
            .iter()
            .position(|item| item.id == id)
        else {
            return Err(anyhow!("未找到 skill：{id}"));
        };
        let removed = state.store.agent.agent_config.skills.installed[remove_idx].clone();

        let install_root = default_skills_storage_dir_path().join("installed");
        let source_path = PathBuf::from(removed.source.value.trim());
        if source_path.starts_with(&install_root) && source_path.exists() {
            fs::remove_dir_all(&source_path)
                .with_context(|| format!("删除 skill 安装目录失败：{}", source_path.display()))?;
            cleanup_empty_skill_install_dirs(&source_path, &install_root)?;
        }
        state
            .store
            .agent
            .agent_config
            .skills
            .installed
            .remove(remove_idx);

        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        state.sync_skill_locks()?;
        Ok(format!("skill 已删除：{id}"))
    }

    pub(in crate::app_state) fn set_skill_enabled(
        self,
        state: &mut TiangongState,
        id: &str,
        enabled: bool,
    ) -> Result<String> {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow!("skill id 不能为空"));
        }
        let Some(skill) = state
            .store
            .agent
            .agent_config
            .skills
            .installed
            .iter_mut()
            .find(|item| item.id == id)
        else {
            return Err(anyhow!("未找到 skill：{id}"));
        };
        skill.enabled = enabled;
        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        state.sync_skill_locks()?;
        Ok(format!("skill 状态已更新：{id} enabled={enabled}"))
    }

    fn resolve_skill_source_path(self, state: &TiangongState, raw_path: &str) -> Result<PathBuf> {
        let path = raw_path.trim();
        if path.is_empty() {
            return Err(anyhow!("skill 路径不能为空"));
        }
        let source_path =
            fs::canonicalize(path).with_context(|| format!("解析 skill 路径失败：{path}"))?;
        if !self.is_allowed_skill_source_path(state, &source_path)? {
            return Err(anyhow!(
                "skill 路径越界：{}，仅允许工作区、skills.dirs 或 ~/.codex/skills 目录",
                source_path.display()
            ));
        }
        Ok(source_path)
    }

    fn is_allowed_skill_source_path(
        self,
        state: &TiangongState,
        source_path: &Path,
    ) -> Result<bool> {
        let workspace = std::env::current_dir()
            .context("读取当前工作目录失败")?
            .canonicalize()
            .context("解析当前工作目录失败")?;
        if source_path.starts_with(&workspace) {
            return Ok(true);
        }

        let mut allowed_roots = state
            .store
            .agent
            .agent_config
            .skills
            .dirs
            .iter()
            .map(|raw: &String| raw.trim())
            .filter(|value: &&str| !value.is_empty())
            .map(PathBuf::from)
            .collect::<Vec<_>>();

        if let Ok(codex_home) = std::env::var("CODEX_HOME") {
            let trimmed = codex_home.trim();
            if !trimmed.is_empty() {
                allowed_roots.push(PathBuf::from(trimmed).join("skills"));
            }
        }

        if let Some(home) = user_home_dir() {
            allowed_roots.push(home.join(".codex").join("skills"));
        }

        Ok(allowed_roots
            .into_iter()
            .filter_map(|path| fs::canonicalize(path).ok())
            .any(|path| source_path.starts_with(path)))
    }
}
