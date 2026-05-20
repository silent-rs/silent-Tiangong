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

        // 兼容旧路径：当用户直接安装仅含 SKILL.md 的目录时，自动补全 skill.toml 后继续安装。
        if !convert_external
            && prepared_source.install_path.join("SKILL.md").exists()
            && !prepared_source.install_path.join("skill.toml").exists()
        {
            prepared_source = prepare_skill_source_for_install(
                &source_path,
                true,
                llm_artifacts.as_ref(),
                convert_env_values,
            )?;
            pre_conversion_notes
                .push("检测到 SKILL.md-only skill，已自动生成 skill.toml 以兼容安装".to_string());
        }

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
        let skill_manifest =
            crate::skill::read_skill_manifest(&prepared_source.install_path.join("skill.toml"))?;
        let skill_id = skill_manifest.id.clone();
        let skill_version = skill_manifest.version.clone();

        // 新平铺安装目录：~/.tiangong/skills/<id>/
        let installed_dir = default_skills_storage_dir_path().join(&skill_id);

        // 读取旧目录的 .env.local（如果已存在），保留环境变量
        let mut old_env: Vec<(String, String)> = Vec::new();
        // 保留原 available 状态（避免覆盖用户禁用状态）
        let preserve_available = if installed_dir.exists() {
            let old_env_path = installed_dir.join(".env.local");
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
            // 读取旧的 available 值
            crate::skill::read_skill_manifest(&installed_dir.join("skill.toml"))
                .ok()
                .map(|m| m.available)
        } else {
            None
        };

        // 如果目标目录已存在，先删除
        if installed_dir.exists() {
            fs::remove_dir_all(&installed_dir)
                .with_context(|| format!("清理旧 skill 目录失败：{}", installed_dir.display()))?;
        }
        ensure_dir(&installed_dir)?;

        let rollback_guard = InstallRollbackGuard::new(installed_dir.clone());

        copy_dir_recursive(&prepared_source.install_path, &installed_dir).with_context(|| {
            format!(
                "复制 skill 到安装目录失败：{} -> {}",
                prepared_source.install_path.display(),
                installed_dir.display()
            )
        })?;

        // 写入 .env.local：合并旧版本 env（旧值为底，新值覆盖）
        {
            let mut merged: std::collections::BTreeMap<String, String> =
                std::collections::BTreeMap::new();
            for (k, v) in &old_env {
                if !k.trim().is_empty() && !v.trim().is_empty() {
                    merged.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            for (k, v) in convert_env_values {
                if !k.trim().is_empty() && !v.trim().is_empty() {
                    merged.insert(k.trim().to_string(), v.trim().replace('\n', "\\n"));
                }
            }
            if !merged.is_empty() {
                let env_lines: Vec<String> =
                    merged.iter().map(|(k, v)| format!("{k}={v}")).collect();
                let env_path = installed_dir.join(".env.local");
                fs::write(&env_path, format!("{}\n", env_lines.join("\n")))
                    .with_context(|| format!("写入 .env.local 失败：{}", env_path.display()))?;
            }
        }

        // 写入 skill.toml.available：保留旧值或设置新值
        let final_available = preserve_available.unwrap_or(enabled);
        crate::skill::write_skill_available(&installed_dir, final_available)?;

        rollback_guard.commit();

        // 刷新注册表并同步内存缓存
        state.services.skill_registry.refresh();
        state.sync_installed_from_registry();
        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        state.persist_agent_configs_only()?;

        let mut message = format!(
            "skill 已安装：{}@{} enabled={}",
            skill_id, skill_version, final_available
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
        audit::append_audit_log(&audit::AuditEntry::new(
            "skill.install",
            &skill_id,
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

        // 从文件系统注册表查找 Skill 目录
        let view = state.services.skill_registry.view();
        let entry = view
            .entries
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("未找到 skill：{id}"))?;
        let skill_dir = entry.dir.clone();

        // 收集该 Skill 托管的 MCP server（用于清理引用）
        let managed_mcp_servers: Vec<String> = state
            .store
            .agent
            .agent_config
            .skills
            .installed
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.managed_mcp_servers.clone())
            .unwrap_or_default();

        // 删除 skills/<id>/ 目录
        if skill_dir.exists() {
            fs::remove_dir_all(&skill_dir)
                .with_context(|| format!("删除 skill 目录失败：{}", skill_dir.display()))?;
        }

        // 驱逐注册表缓存并同步内存
        state.services.skill_registry.invalidate(id);
        state.services.skill_registry.refresh();
        state.sync_installed_from_registry();

        // 清理该 skill 托管的 MCP server（引用计数为 0 时移除）
        for mcp_id in &managed_mcp_servers {
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
        state.persist_agent_configs_only()?;
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
        // 只修改 skills/<id>/skill.toml 的 available 字段
        state
            .services
            .skill_registry
            .set_available(id, enabled)
            .with_context(|| format!("设置 skill available 失败：{id}"))?;

        // 同步内存缓存
        state.sync_installed_from_registry();
        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_agent_configs_only()?;
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
        if source
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        {
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
        let entries: Vec<_> = fs::read_dir(&import_dir)?.filter_map(|e| e.ok()).collect();
        if entries.len() == 1 && entries[0].file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let inner = entries[0].path();
            if inner.join("SKILL.md").exists() || inner.join("skill.toml").exists() {
                return Ok(inner);
            }
        }

        Ok(import_dir)
    }
}
