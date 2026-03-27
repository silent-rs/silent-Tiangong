use super::super::*;
use crate::app_state::audit;
use crate::app_state::support::InstallRollbackGuard;

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

        // 从 skill.toml 中读取显式声明的环境变量
        let mut env_vars = Vec::new();
        let skill_toml_path = source_path.join("skill.toml");
        if let Ok(raw) = fs::read_to_string(&skill_toml_path) {
            #[derive(serde::Deserialize, Default)]
            struct TomlEnvCheck {
                #[serde(default)]
                requires: TomlRequires,
            }
            #[derive(serde::Deserialize, Default)]
            struct TomlRequires {
                #[serde(default)]
                env: Vec<String>,
            }
            if let Ok(parsed) = toml::from_str::<TomlEnvCheck>(&raw) {
                env_vars.extend(parsed.requires.env);
            }
        }

        // 代码扫描检测额外的环境变量
        if convert_external {
            let analysis = analyze_external_skill(&source_path)?;
            for v in analysis.env_vars {
                if !env_vars.contains(&v) {
                    env_vars.push(v);
                }
            }
            // 合并依赖信息
            let missing_env_vars = env_vars
                .iter()
                .filter(|key| std::env::var_os(key.as_str()).is_none())
                .cloned()
                .collect::<Vec<_>>();
            return Ok(SkillInstallInspection {
                dependencies: analysis.dependencies,
                env_vars,
                missing_env_vars,
            });
        }

        let missing_env_vars = env_vars
            .iter()
            .filter(|key| std::env::var_os(key.as_str()).is_none())
            .cloned()
            .collect::<Vec<_>>();
        Ok(SkillInstallInspection {
            dependencies: Vec::new(),
            env_vars,
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
                let model_client = SingleProviderClient::new(
                    state.store.provider.models_config.to_chat_provider_config(),
                );
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

        // 版本更新：读取旧版本的 .env.local 以便保留
        let mut old_env: Vec<(String, String)> = Vec::new();
        if let Some(existing) = state
            .store
            .agent
            .agent_config
            .skills
            .installed
            .iter()
            .find(|item| item.id == skill.id)
        {
            let old_env_path =
                std::path::Path::new(&existing.source.value).join(".env.local");
            if let Ok(content) = fs::read_to_string(&old_env_path) {
                for line in content.lines() {
                    let line = line.trim();
                    if !line.is_empty()
                        && !line.starts_with('#')
                        && let Some((k, v)) = line.split_once('=')
                    {
                        old_env.push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
            }
            // 清理旧版本目录
            let old_dir = std::path::Path::new(&existing.source.value);
            if old_dir.exists() {
                let _ = fs::remove_dir_all(old_dir);
                // 如果父目录（skill id 目录）为空也清理
                if let Some(parent) = old_dir.parent() {
                    let _ = fs::remove_dir(parent); // 只在空目录时成功
                }
            }
            // 移除旧版本注册信息（允许重新安装/更新）
            state
                .store
                .agent
                .agent_config
                .skills
                .installed
                .retain(|item| item.id != skill.id);
        }

        let installed_dir = default_skills_storage_dir_path()
            .join("installed")
            .join(&skill.id)
            .join(&skill.version);
        if installed_dir.exists() {
            // 版本目录已存在时覆盖安装
            let _ = fs::remove_dir_all(&installed_dir);
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

        // 写入 .env.local：合并旧版本 env（旧值为底，新值覆盖）
        {
            let mut merged: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
            // 先填入旧版本的值
            for (k, v) in &old_env {
                if !k.trim().is_empty() && !v.trim().is_empty() {
                    merged.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            // 新值覆盖
            for (k, v) in convert_env_values {
                if !k.trim().is_empty() && !v.trim().is_empty() {
                    merged.insert(k.trim().to_string(), v.trim().replace('\n', "\\n"));
                }
            }
            if !merged.is_empty() {
                let env_lines: Vec<String> = merged
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                let env_path = installed_dir.join(".env.local");
                fs::write(&env_path, format!("{}\n", env_lines.join("\n")))
                    .with_context(|| format!("写入 .env.local 失败：{}", env_path.display()))?;
            }
        }

        let rollback_guard = InstallRollbackGuard::new(installed_dir.clone());

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
        rollback_guard.commit();
        audit::append_audit_log(&audit::AuditEntry::new(
            "skill.install",
            &skill.id,
            &message,
            true,
        ));
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

        // 清理该 skill 托管的 MCP server（引用计数为 0 时移除）
        for mcp_id in &removed.managed_mcp_servers {
            let still_referenced = state
                .store
                .agent
                .agent_config
                .skills
                .installed
                .iter()
                .any(|s| s.managed_mcp_servers.contains(mcp_id));
            if !still_referenced {
                state
                    .store
                    .agent
                    .agent_config
                    .mcp
                    .servers
                    .retain(|server| server.name != *mcp_id);
            }
        }

        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        state.sync_skill_locks()?;
        audit::append_audit_log(&audit::AuditEntry::new(
            "skill.remove",
            id,
            &format!("skill 已删除：{id}"),
            true,
        ));
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
        audit::append_audit_log(&audit::AuditEntry::new(
            "skill.toggle",
            id,
            &format!("enabled={enabled}"),
            true,
        ));
        Ok(format!("skill 状态已更新：{id} enabled={enabled}"))
    }

    fn resolve_skill_source_path(self, _state: &TiangongState, raw_path: &str) -> Result<PathBuf> {
        let path = raw_path.trim();
        if path.is_empty() {
            return Err(anyhow!("skill 路径不能为空"));
        }

        let source = Path::new(path);

        // 支持 zip 压缩包：解压到 ~/.tiangong/skills/imported/ 后使用解压目录
        if source.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip")) {
            let source_path = fs::canonicalize(source)
                .with_context(|| format!("解析 skill 压缩包路径失败：{path}"))?;
            return Self::extract_skill_zip(&source_path);
        }

        // 普通目录
        let source_path =
            fs::canonicalize(path).with_context(|| format!("解析 skill 路径失败：{path}"))?;
        if !source_path.is_dir() {
            return Err(anyhow!("skill 路径不是目录：{}", source_path.display()));
        }
        Ok(source_path)
    }

    /// 解压 skill zip 包到 ~/.tiangong/skills/imported/
    fn extract_skill_zip(zip_path: &Path) -> Result<PathBuf> {
        use std::io;

        let file = fs::File::open(zip_path)
            .with_context(|| format!("打开 skill 压缩包失败：{}", zip_path.display()))?;
        let mut archive = zip::ZipArchive::new(io::BufReader::new(file))
            .with_context(|| format!("读取 zip 文件失败：{}", zip_path.display()))?;

        // 解压目标：~/.tiangong/skills/imported/<zip名称>/
        let zip_stem = zip_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("skill");
        let import_dir = default_skills_storage_dir_path()
            .join("imported")
            .join(zip_stem);

        // 如果已存在先删除
        if import_dir.exists() {
            fs::remove_dir_all(&import_dir)
                .with_context(|| format!("清理旧导入目录失败：{}", import_dir.display()))?;
        }
        fs::create_dir_all(&import_dir)
            .with_context(|| format!("创建导入目录失败：{}", import_dir.display()))?;

        // 解压所有文件
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let entry_path = match entry.enclosed_name() {
                Some(p) => p.to_owned(),
                None => continue,
            };

            // 跳过 __MACOSX 等隐藏目录
            if entry_path
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with("__"))
            {
                continue;
            }

            let target = import_dir.join(&entry_path);

            if entry.is_dir() {
                fs::create_dir_all(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = fs::File::create(&target)?;
                io::copy(&mut entry, &mut out)?;
            }
        }

        // 检查解压后的目录结构：如果 zip 内只有一个子目录，使用该子目录
        let entries: Vec<_> = fs::read_dir(&import_dir)?
            .filter_map(|e| e.ok())
            .collect();
        if entries.len() == 1 && entries[0].file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let inner = entries[0].path();
            if inner.join("SKILL.md").exists() || inner.join("skill.toml").exists() {
                return Ok(inner);
            }
        }

        Ok(import_dir)
    }

}
