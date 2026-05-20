use std::fs;

use anyhow::Result;

use super::super::*;
use super::common::{make_installed_skill, with_isolated_state};
use crate::agent_config::{McpServerConfig, SkillMcpRequirementConfig};

#[test]
fn repository_persist_to_disk_round_trips_split_configs_and_sessions() -> Result<()> {
    with_isolated_state("tiangong-repository-roundtrip", |paths, state| {
        state.store.provider.model_list = vec!["glm-test".to_string(), "glm-4.7".to_string()];
        state.store.agent.agent_config.skills.max_matches = 9;
        state.store.agent.agent_config.skills.dirs = vec![paths.workspace.display().to_string()];
        state.store.agent.agent_config.mcp.timeout_ms = 23_000;
        state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .push(McpServerConfig {
                name: "demo".to_string(),
                transport: McpTransportMode::Http,
                command: String::new(),
                args: Vec::new(),
                endpoint: "http://127.0.0.1:8080/mcp".to_string(),
                auth_header: String::new(),
                headers: Default::default(),
                env: Default::default(),
                cwd: String::new(),
                enabled: true,
                tags: vec!["demo".to_string()],
            });

        let session = Session::new("第二会话");
        state.store.session.active_session_id = session.id.clone();
        state.store.session.sessions.push(session.clone());

        state.persist_to_disk()?;

        let loaded = state
            .services
            .repository
            .load_from_disk()?
            .expect("应能从磁盘回读状态");

        assert_eq!(loaded.active_session_id, session.id);
        assert_eq!(loaded.sessions.len(), state.store.session.sessions.len());
        assert_eq!(
            loaded.model_list.first().map(String::as_str),
            Some("glm-test")
        );
        let loaded_agent = loaded
            .agent_config
            .expect("应从 skills.json/mcp.json 恢复 agent 配置");
        assert_eq!(loaded_agent.skills.max_matches, 9);
        assert_eq!(loaded_agent.skills.dirs.len(), 1);
        assert_eq!(loaded_agent.mcp.timeout_ms, 23_000);
        assert_eq!(loaded_agent.mcp.servers.len(), 1);

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
            paths
                .fake_home
                .join(".tiangong")
                .join("sessions")
                .join(format!("{}.json", session.id))
                .exists()
        );
        Ok(())
    })
}

#[test]
fn load_from_disk_prefers_most_recent_session_when_app_storage_missing() -> Result<()> {
    with_isolated_state(
        "tiangong-repository-recover-latest-without-app",
        |paths, state| {
            // 清除 load_or_default 创建的默认会话文件
            let sessions_dir = paths.fake_home.join(".tiangong").join("sessions");
            if sessions_dir.exists() {
                for entry in fs::read_dir(&sessions_dir)? {
                    fs::remove_file(entry?.path())?;
                }
            }

            state.store.session.sessions.clear();

            let mut older = Session::new("较早会话");
            older.created_at = "2026-04-22 09:00:00.000000".to_string();
            older.updated_at = "2026-04-22 09:05:00.000000".to_string();

            let mut newer = Session::new("最近会话");
            newer.created_at = "2026-04-23 10:00:00.000000".to_string();
            newer.updated_at = "2026-04-23 10:20:00.000000".to_string();

            state.store.session.active_session_id = older.id.clone();
            state.store.session.sessions = vec![older.clone(), newer.clone()];
            state.persist_to_disk()?;

            fs::remove_file(paths.fake_home.join(".tiangong").join("app.json"))?;

            let loaded = state
                .services
                .repository
                .load_from_disk()?
                .expect("应能从 session 文件恢复状态");

            assert_eq!(loaded.active_session_id, newer.id);
            assert_eq!(loaded.sessions.len(), 2);
            Ok(())
        },
    )
}

#[test]
fn load_from_disk_prefers_most_recent_session_when_active_session_is_invalid() -> Result<()> {
    with_isolated_state(
        "tiangong-repository-recover-latest-invalid-active",
        |paths, state| {
            // 清除 load_or_default 创建的默认会话文件
            let sessions_dir = paths.fake_home.join(".tiangong").join("sessions");
            if sessions_dir.exists() {
                for entry in fs::read_dir(&sessions_dir)? {
                    fs::remove_file(entry?.path())?;
                }
            }

            state.store.session.sessions.clear();

            let mut older = Session::new("较早会话");
            older.created_at = "2026-04-22 09:00:00.000000".to_string();
            older.updated_at = "2026-04-22 09:05:00.000000".to_string();

            let mut newer = Session::new("最近会话");
            newer.created_at = "2026-04-23 10:00:00.000000".to_string();
            newer.updated_at = "2026-04-23 10:20:00.000000".to_string();

            state.store.session.sessions = vec![older, newer.clone()];
            state.store.session.active_session_id = newer.id.clone();
            state.persist_to_disk()?;

            let app_path = paths.fake_home.join(".tiangong").join("app.json");
            let mut app_json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&app_path)?)?;
            app_json["active_session_id"] =
                serde_json::Value::String("missing-session-id".to_string());
            fs::write(&app_path, serde_json::to_string_pretty(&app_json)?)?;

            let loaded = state
                .services
                .repository
                .load_from_disk()?
                .expect("应能在 active_session_id 失效时恢复状态");

            assert_eq!(loaded.active_session_id, newer.id);
            assert_eq!(loaded.sessions.len(), 2);
            Ok(())
        },
    )
}

#[test]
fn load_from_disk_filters_child_sessions_from_session_list_state() -> Result<()> {
    with_isolated_state(
        "tiangong-repository-filter-child-sessions",
        |paths, state| {
            // 清除 load_or_default 创建的默认会话文件
            let sessions_dir = paths.fake_home.join(".tiangong").join("sessions");
            if sessions_dir.exists() {
                for entry in fs::read_dir(&sessions_dir)? {
                    fs::remove_file(entry?.path())?;
                }
            }

            state.store.session.sessions.clear();

            let mut parent = Session::new("主会话");
            parent.updated_at = "2026-04-30 09:00:00.000000".to_string();

            let mut child = Session::new("Worker 子会话");
            child.parent_session_id = Some(parent.id.clone());
            child.updated_at = "2026-04-30 09:10:00.000000".to_string();

            state.store.session.active_session_id = child.id.clone();
            state.store.session.sessions = vec![parent.clone(), child];
            state.persist_to_disk()?;

            let loaded = state
                .services
                .repository
                .load_from_disk()?
                .expect("应能从磁盘回读状态");

            assert_eq!(loaded.active_session_id, parent.id);
            assert_eq!(loaded.sessions.len(), 1);
            assert_eq!(loaded.sessions[0].title, "主会话");
            assert!(loaded.sessions[0].parent_session_id.is_none());
            Ok(())
        },
    )
}

#[test]
fn sync_mcp_dependency_lock_writes_expected_ref_counts() -> Result<()> {
    with_isolated_state("tiangong-mcp-dependency-lock", |paths, state| {
        state.store.agent.agent_config.skills.installed = vec![
            make_installed_skill(
                "alpha",
                "2026-03-06 10:00:00",
                vec![
                    SkillMcpRequirementConfig {
                        id: "m1".to_string(),
                        source: String::new(),
                        package: "pkg-a".to_string(),
                        version: "1.0.0".to_string(),
                    },
                    SkillMcpRequirementConfig {
                        id: "m2".to_string(),
                        source: String::new(),
                        package: "pkg-b".to_string(),
                        version: String::new(),
                    },
                ],
            ),
            make_installed_skill(
                "beta",
                "2026-03-06 11:00:00",
                vec![SkillMcpRequirementConfig {
                    id: "m3".to_string(),
                    source: String::new(),
                    package: "pkg-a".to_string(),
                    version: "1.0.0".to_string(),
                }],
            ),
        ];

        state.sync_mcp_dependency_lock()?;

        let mcp_lock_path = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join("mcp-lock.json");

        let mcp_lock: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(mcp_lock_path)?)?;

        assert!(
            !paths
                .fake_home
                .join(".tiangong")
                .join("skills")
                .join("skills-lock.json")
                .exists()
        );
        assert_eq!(
            mcp_lock
                .get("pkg-a@1.0.0")
                .and_then(|item| item.get("ref_count"))
                .and_then(|value| value.as_u64()),
            Some(2)
        );
        assert_eq!(
            mcp_lock
                .get("pkg-b")
                .and_then(|item| item.get("ref_count"))
                .and_then(|value| value.as_u64()),
            Some(1)
        );
        Ok(())
    })
}
