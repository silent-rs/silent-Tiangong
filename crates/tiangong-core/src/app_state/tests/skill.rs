use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use super::common::with_isolated_state;
use crate::agent_config::{McpServerConfig, McpTransportMode};
use crate::app_state::TiangongState;
use crate::skill::read_skill_manifest;

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn install_skill_saves_into_user_skills_dir_and_recognizable() -> Result<()> {
    with_isolated_state("tiangong-skill-install", |paths, state| {
        let nonce = scru128::new().to_string();
        let source_dir = paths.workspace.join("demo-skill-src");
        fs::create_dir_all(&source_dir)?;

        let skill_id = format!("demo-skill-{nonce}");
        let skill_md = source_dir.join("SKILL.md");
        let skill_toml = source_dir.join("skill.toml");
        fs::write(
            &skill_md,
            "# Demo Skill\n用于测试安装行为是否写入 ~/.tiangong/skills。\n",
        )?;
        fs::write(
            &skill_toml,
            format!(
                "id = \"{skill_id}\"\nname = \"Demo Skill\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                source_dir.display()
            ),
        )?;

        let install_message =
            state.install_local_skill(source_dir.to_str().unwrap_or_default(), true)?;
        assert!(install_message.contains("skill 已安装"));

        let installed = state
            .installed_skills()
            .iter()
            .find(|skill| skill.id == skill_id)
            .cloned()
            .expect("安装后应可在配置中找到 skill");
        let installed_path = PathBuf::from(installed.source.value.clone());
        // 新平铺布局：skills/<id>/（不含 version 子目录）
        let expected_prefix = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join(&skill_id);
        assert!(installed_path.starts_with(&expected_prefix));
        assert!(installed_path.join("SKILL.md").exists());
        assert!(paths.fake_home.join(".tiangong").join("app.json").exists());
        assert!(
            paths
                .fake_home
                .join(".tiangong")
                .join("skills.json")
                .exists()
        );
        assert!(paths.fake_home.join(".tiangong").join("mcp.json").exists());
        assert!(
            !paths
                .fake_home
                .join(".tiangong")
                .join("skills")
                .join("skills-lock.json")
                .exists()
        );

        state.remove_skill(&skill_id)?;
        assert!(!installed_path.exists());
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn startup_migrates_legacy_skill_layout_and_writes_backups() -> Result<()> {
    with_isolated_state("tiangong-skill-migration", |paths, state| {
        let nonce = scru128::new().to_string();
        let skill_id = format!("legacy-skill-{nonce}");
        let legacy_version = "0.1.0";

        let legacy_skill_dir = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join("installed")
            .join(&skill_id)
            .join(legacy_version);
        fs::create_dir_all(&legacy_skill_dir)?;
        fs::write(
            legacy_skill_dir.join("SKILL.md"),
            "# Legacy Skill\n用于测试旧布局迁移。\n",
        )?;
        fs::write(
            legacy_skill_dir.join("skill.toml"),
            format!(
                "id = \"{skill_id}\"\nname = \"Legacy Skill\"\nversion = \"{legacy_version}\"\nentry = \"SKILL.md\"\navailable = true\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                legacy_skill_dir.display()
            ),
        )?;

        let skills_json_path = paths.fake_home.join(".tiangong").join("skills.json");
        let legacy_skills_json = json!({
            "enabled": true,
            "dirs": [],
            "max_matches": 3,
            "installed": [
                {
                    "id": skill_id,
                    "name": "Legacy Skill",
                    "version": legacy_version,
                    "description": "",
                    "entry": "SKILL.md",
                    "enabled": false,
                    "installed_at": "",
                    "managed_mcp_servers": [],
                    "source": {
                        "kind": "local",
                        "value": legacy_skill_dir.display().to_string()
                    },
                    "requires_mcp": [],
                    "permissions": {
                        "fs_read": ["./**"],
                        "fs_write": [],
                        "cmd_exec": [],
                        "net": []
                    }
                }
            ]
        });
        fs::write(
            &skills_json_path,
            serde_json::to_string_pretty(&legacy_skills_json)?,
        )?;

        let skills_lock_path = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join("skills-lock.json");
        fs::write(&skills_lock_path, "{}")?;

        *state = TiangongState::load_or_default();

        let migrated_dir = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join(&skill_id);
        assert!(migrated_dir.join("skill.toml").exists());
        let migrated_manifest = read_skill_manifest(&migrated_dir.join("skill.toml"))?;
        assert!(!migrated_manifest.available);

        assert!(
            paths
                .fake_home
                .join(".tiangong")
                .join("skills.json.legacy")
                .exists()
        );
        assert!(
            paths
                .fake_home
                .join(".tiangong")
                .join("skills")
                .join("skills-lock.json.legacy")
                .exists()
        );
        assert!(
            !paths
                .fake_home
                .join(".tiangong")
                .join("skills")
                .join("skills-lock.json")
                .exists()
        );
        assert!(
            paths
                .fake_home
                .join(".tiangong")
                .join("skills")
                .join("installed.legacy")
                .exists()
        );
        assert!(
            !paths
                .fake_home
                .join(".tiangong")
                .join("skills")
                .join("installed")
                .exists()
        );
        assert!(
            !paths
                .fake_home
                .join(".tiangong")
                .join("skills")
                .join("migration-failed.lock")
                .exists()
        );

        let installed = state.installed_skills();
        let migrated = installed
            .iter()
            .find(|s| s.id == skill_id)
            .expect("迁移后 skill 应可见");
        assert!(!migrated.enabled);
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn standalone_skills_lock_does_not_trigger_legacy_migration() -> Result<()> {
    with_isolated_state("tiangong-skill-lock-only", |paths, state| {
        let skills_root = paths.fake_home.join(".tiangong").join("skills");
        fs::create_dir_all(&skills_root)?;
        fs::write(skills_root.join("skills-lock.json"), "{}")?;

        *state = TiangongState::load_or_default();

        assert!(
            !skills_root.join("skills-lock.json.legacy").exists(),
            "skills-lock 只能作为旧布局伴随文件迁移，不能单独触发迁移"
        );
        assert!(
            !skills_root.join("migration-failed.lock").exists(),
            "单独存在 skills-lock 不应进入迁移失败路径"
        );
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn manual_copy_skill_is_visible_after_next_startup_scan() -> Result<()> {
    with_isolated_state("tiangong-skill-manual-copy", |paths, state| {
        let nonce = scru128::new().to_string();
        let skill_id = format!("manual-skill-{nonce}");
        let manual_dir = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join(&skill_id);
        fs::create_dir_all(&manual_dir)?;
        fs::write(
            manual_dir.join("SKILL.md"),
            "# Manual Skill\n用于测试手动拷贝后的扫描可见性。\n",
        )?;
        fs::write(
            manual_dir.join("skill.toml"),
            format!(
                "id = \"{skill_id}\"\nname = \"Manual Skill\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\navailable = true\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                manual_dir.display()
            ),
        )?;

        *state = TiangongState::load_or_default();

        assert!(state.installed_skills().iter().any(|s| s.id == skill_id));
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn manual_delete_skill_is_invisible_after_next_startup_scan() -> Result<()> {
    with_isolated_state("tiangong-skill-manual-delete", |paths, state| {
        let nonce = scru128::new().to_string();
        let source_dir = paths.workspace.join("manual-delete-skill-src");
        fs::create_dir_all(&source_dir)?;

        let skill_id = format!("manual-delete-skill-{nonce}");
        fs::write(source_dir.join("SKILL.md"), "# Delete Skill\n")?;
        fs::write(
            source_dir.join("skill.toml"),
            format!(
                "id = \"{skill_id}\"\nname = \"Delete Skill\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                source_dir.display()
            ),
        )?;

        state.install_local_skill(source_dir.to_str().unwrap_or_default(), true)?;
        assert!(state.installed_skills().iter().any(|s| s.id == skill_id));

        let installed_dir = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join(&skill_id);
        fs::remove_dir_all(&installed_dir)?;

        *state = TiangongState::load_or_default();

        assert!(!state.installed_skills().iter().any(|s| s.id == skill_id));
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn disabled_state_persists_via_skill_toml_available_false() -> Result<()> {
    with_isolated_state("tiangong-skill-disable-persist", |paths, state| {
        let nonce = scru128::new().to_string();
        let source_dir = paths.workspace.join("disable-persist-skill-src");
        fs::create_dir_all(&source_dir)?;

        let skill_id = format!("disable-persist-skill-{nonce}");
        fs::write(source_dir.join("SKILL.md"), "# Disable Skill\n")?;
        fs::write(
            source_dir.join("skill.toml"),
            format!(
                "id = \"{skill_id}\"\nname = \"Disable Skill\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = [{{ id = \"tool\", source = \"npm\", package = \"demo-mcp\", version = \"1.0.0\" }}]\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                source_dir.display()
            ),
        )?;

        state.install_local_skill(source_dir.to_str().unwrap_or_default(), true)?;
        assert!(state.installed_skills().iter().any(|s| s.id == skill_id));

        state.set_skill_enabled(&skill_id, false)?;

        let installed_dir = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join(&skill_id);
        let manifest = read_skill_manifest(&installed_dir.join("skill.toml"))?;
        assert!(!manifest.available);

        // 禁用后仍应保留在 installed 缓存中（用于 GUI/Tauri 列表展示与重新启用）
        let installed = state.installed_skills();
        let disabled_skill = installed
            .iter()
            .find(|s| s.id == skill_id)
            .expect("禁用后 skill 仍应可见");
        assert!(!disabled_skill.enabled);

        // mcp-lock 应按“已安装 skill”统计依赖引用，禁用不应丢失引用
        let mcp_lock_path = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join("mcp-lock.json");
        let mcp_lock: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(mcp_lock_path)?)?;
        assert_eq!(
            mcp_lock
                .get("demo-mcp@1.0.0")
                .and_then(|item| item.get("ref_count"))
                .and_then(|value| value.as_u64()),
            Some(1)
        );

        *state = TiangongState::load_or_default();
        let installed = state.installed_skills();
        let disabled_skill = installed
            .iter()
            .find(|s| s.id == skill_id)
            .expect("重启后禁用 skill 仍应可见");
        assert!(!disabled_skill.enabled);
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn install_skill_md_only_dir_still_works_for_compatibility() -> Result<()> {
    with_isolated_state("tiangong-skill-md-only", |paths, state| {
        let nonce = scru128::new().to_string();
        let source_dir = paths.workspace.join(format!("md-only-{nonce}"));
        fs::create_dir_all(&source_dir)?;

        // 只提供 SKILL.md（不提供 skill.toml）
        fs::write(
            source_dir.join("SKILL.md"),
            "# MD Only Skill\n用于测试 SKILL.md-only 安装兼容。\n",
        )?;

        let message = state.install_local_skill(source_dir.to_str().unwrap_or_default(), true)?;
        assert!(message.contains("skill 已安装"));

        let installed_skills = state.installed_skills();
        let installed = installed_skills
            .iter()
            .find(|s| s.name == "MD Only Skill")
            .expect("应能安装并识别 SKILL.md-only skill");
        let installed_dir = PathBuf::from(&installed.source.value);
        assert!(installed_dir.join("SKILL.md").exists());
        // 兼容路径会自动补全 skill.toml，供注册表/启停逻辑使用
        assert!(installed_dir.join("skill.toml").exists());
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn migration_failure_writes_fail_lock_and_keeps_legacy_files() -> Result<()> {
    with_isolated_state("tiangong-skill-migration-fail", |paths, state| {
        let nonce = scru128::new().to_string();
        let skill_id = format!("legacy-fail-skill-{nonce}");

        let skills_root = paths.fake_home.join(".tiangong").join("skills");
        fs::create_dir_all(&skills_root)?;
        // 构造非法旧布局：installed 应为目录，这里人为写成文件，触发迁移失败。
        fs::write(skills_root.join("installed"), "invalid legacy layout")?;

        let skills_json_path = paths.fake_home.join(".tiangong").join("skills.json");
        let legacy_skills_json = json!({
            "enabled": true,
            "dirs": [],
            "max_matches": 3,
            "installed": [
                {
                    "id": skill_id,
                    "name": "Legacy Fail Skill",
                    "version": "0.1.0",
                    "description": "",
                    "entry": "SKILL.md",
                    "enabled": true,
                    "installed_at": "",
                    "managed_mcp_servers": [],
                    "source": {
                        "kind": "local",
                        "value": "legacy"
                    },
                    "requires_mcp": [],
                    "permissions": {
                        "fs_read": ["./**"],
                        "fs_write": [],
                        "cmd_exec": [],
                        "net": []
                    }
                }
            ]
        });
        fs::write(
            &skills_json_path,
            serde_json::to_string_pretty(&legacy_skills_json)?,
        )?;

        let legacy_lock_path = skills_root.join("skills-lock.json");
        fs::write(&legacy_lock_path, "{}")?;

        *state = TiangongState::load_or_default();

        let fail_lock = skills_root.join("migration-failed.lock");
        assert!(fail_lock.exists());
        // 失败时不删除旧文件
        assert!(skills_json_path.exists());
        assert!(legacy_lock_path.exists());
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn refresh_skills_detects_manual_copy_without_restart() -> Result<()> {
    with_isolated_state("tiangong-skill-refresh-copy", |paths, state| {
        let nonce = scru128::new().to_string();
        let skill_id = format!("refresh-copy-skill-{nonce}");
        let manual_dir = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join(&skill_id);
        fs::create_dir_all(&manual_dir)?;
        fs::write(
            manual_dir.join("SKILL.md"),
            "# Refresh Copy Skill\n用于测试 refresh 重扫。\n",
        )?;
        fs::write(
            manual_dir.join("skill.toml"),
            format!(
                "id = \"{skill_id}\"\nname = \"Refresh Copy Skill\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\navailable = true\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                manual_dir.display()
            ),
        )?;

        assert!(!state.installed_skills().iter().any(|s| s.id == skill_id));
        let msg = state.refresh_skills()?;
        assert!(msg.contains("skills 已刷新"));
        assert!(state.installed_skills().iter().any(|s| s.id == skill_id));
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn refresh_skills_detects_manual_delete_without_restart() -> Result<()> {
    with_isolated_state("tiangong-skill-refresh-delete", |paths, state| {
        let nonce = scru128::new().to_string();
        let source_dir = paths.workspace.join("refresh-delete-skill-src");
        fs::create_dir_all(&source_dir)?;

        let skill_id = format!("refresh-delete-skill-{nonce}");
        fs::write(source_dir.join("SKILL.md"), "# Refresh Delete Skill\n")?;
        fs::write(
            source_dir.join("skill.toml"),
            format!(
                "id = \"{skill_id}\"\nname = \"Refresh Delete Skill\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                source_dir.display()
            ),
        )?;

        state.install_local_skill(source_dir.to_str().unwrap_or_default(), true)?;
        assert!(state.installed_skills().iter().any(|s| s.id == skill_id));

        let installed_dir = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join(&skill_id);
        fs::remove_dir_all(&installed_dir)?;

        let msg = state.refresh_skills()?;
        assert!(msg.contains("skills 已刷新"));
        assert!(!state.installed_skills().iter().any(|s| s.id == skill_id));
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn gc_skills_dry_run_reports_orphans_without_removing() -> Result<()> {
    with_isolated_state("tiangong-skill-gc-dry-run", |paths, state| {
        let orphan_server = "skill::missing-skill::tool";
        state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .push(test_mcp_server(orphan_server));

        let mcp_lock_path = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join("mcp-lock.json");
        fs::create_dir_all(mcp_lock_path.parent().expect("mcp-lock 应有父目录"))?;
        fs::write(
            &mcp_lock_path,
            serde_json::json!({
                "stale-pkg@9.9.9": {
                    "path": "",
                    "ref_count": 1,
                    "installed_at": ""
                }
            })
            .to_string(),
        )?;

        let msg = state.gc_skills(false)?;
        assert!(msg.contains(orphan_server));
        assert!(msg.contains("stale-pkg@9.9.9"));
        assert!(
            state
                .store
                .agent
                .agent_config
                .mcp
                .servers
                .iter()
                .any(|server| server.name == orphan_server)
        );
        let lock: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(&mcp_lock_path)?)?;
        assert!(lock.contains_key("stale-pkg@9.9.9"));
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn gc_skills_apply_removes_only_orphan_mcp_state() -> Result<()> {
    with_isolated_state("tiangong-skill-gc-apply", |paths, state| {
        let nonce = scru128::new().to_string();
        let skill_id = format!("gc-active-skill-{nonce}");
        let skill_dir = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join(&skill_id);
        fs::create_dir_all(&skill_dir)?;
        fs::write(skill_dir.join("SKILL.md"), "# GC Active Skill\n")?;
        fs::write(
            skill_dir.join("skill.toml"),
            format!(
                "id = \"{skill_id}\"\nname = \"GC Active Skill\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\navailable = true\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = [{{ id = \"tool\", source = \"npm\", package = \"active-pkg\", version = \"1.0.0\" }}]\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                skill_dir.display()
            ),
        )?;

        let active_server = format!("skill::{skill_id}::tool");
        let orphan_server = "skill::deleted-skill::tool";
        state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .push(test_mcp_server(&active_server));
        state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .push(test_mcp_server(orphan_server));

        let mcp_lock_path = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join("mcp-lock.json");
        fs::create_dir_all(mcp_lock_path.parent().expect("mcp-lock 应有父目录"))?;
        fs::write(
            &mcp_lock_path,
            serde_json::json!({
                "active-pkg@1.0.0": {
                    "path": "",
                    "ref_count": 1,
                    "installed_at": ""
                },
                "stale-pkg@9.9.9": {
                    "path": "",
                    "ref_count": 1,
                    "installed_at": ""
                }
            })
            .to_string(),
        )?;

        let msg = state.gc_skills(true)?;
        assert!(msg.contains(orphan_server));
        assert!(msg.contains("stale-pkg@9.9.9"));
        assert!(
            state
                .store
                .agent
                .agent_config
                .mcp
                .servers
                .iter()
                .any(|server| server.name == active_server)
        );
        assert!(
            !state
                .store
                .agent
                .agent_config
                .mcp
                .servers
                .iter()
                .any(|server| server.name == orphan_server)
        );
        let lock: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(&mcp_lock_path)?)?;
        assert!(lock.contains_key("active-pkg@1.0.0"));
        assert!(!lock.contains_key("stale-pkg@9.9.9"));
        Ok(())
    })
}

#[test]
#[ignore = "Phase 4: skill management migrating to plugin"]
fn doctor_skills_reports_registry_issues_and_orphans() -> Result<()> {
    with_isolated_state("tiangong-skill-doctor", |paths, state| {
        let skills_root = paths.fake_home.join(".tiangong").join("skills");
        let broken_dir = skills_root.join("broken-skill");
        fs::create_dir_all(&broken_dir)?;
        fs::write(broken_dir.join("SKILL.md"), "# Broken Skill\n")?;

        let orphan_server = "skill::missing-skill::tool";
        state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .push(test_mcp_server(orphan_server));

        let report = state.doctor_skills()?;
        assert!(report.contains("registry_issue"));
        assert!(report.contains("MissingManifest"));
        assert!(report.contains(orphan_server));
        Ok(())
    })
}

fn test_mcp_server(name: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransportMode::Stdio,
        command: "echo".to_string(),
        args: Vec::new(),
        endpoint: String::new(),
        auth_header: String::new(),
        headers: Default::default(),
        env: Default::default(),
        enabled: true,
        tags: Vec::new(),
    }
}
