use std::fs;

use anyhow::Result;

use super::super::*;
use super::common::with_isolated_state;

#[test]
#[ignore = "Phase 4: 扩展能力配置已脱离 agent_config，持久化契约由各自 plugin 自治"]
fn repository_persist_to_disk_round_trips_split_configs_and_sessions() -> Result<()> {
    with_isolated_state("tiangong-repository-roundtrip", |paths, state| {
        state.store.provider.model_list = vec!["glm-test".to_string(), "glm-4.7".to_string()];

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

        let app_path = paths.fake_home.join(".tiangong").join("app.json");
        assert!(app_path.exists());
        let app_json: serde_json::Value = serde_json::from_str(&fs::read_to_string(&app_path)?)?;
        assert!(app_json.get("agent_config").is_some());
        // 扩展能力配置已脱离 AgentConfig（由各自 plugin 自管），
        // app.json 的 agent_config 不再包含遗留的扩展能力字段。
        assert!(app_json["agent_config"].get("mcp").is_none());
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

/// 回归测试：core 持久化必须剥离 legacy 外部能力字段，且不丢失 agent runtime 字段。
///
/// 场景：旧版 app.json 的 `agent_config` 可能含已脱离 core 的 `mcp` 字段。
/// 重新加载并回写时，core 必须保证：
/// 1. 新 app.json 的 `agent_config` 不再含 `mcp`（core 不持有/不回写外部能力配置）；
/// 2. runtime 字段（trust_mode / custom_system_prompt / reasoning_effort）完整保留。
///
/// 此测试未忽略——它锁住 `serialize_app_payload_stripped_external_configs` 的核心契约，
/// 避免后续 refactor 误把外部配置写回 core app state。
#[test]
fn persist_strips_legacy_external_config_fields_and_keeps_runtime_fields() -> Result<()> {
    use tiangong_core::permission::TrustMode;

    with_isolated_state("tiangong-persist-strip-external-config", |paths, state| {
        // 1. 设置非默认的 agent runtime 字段，确保后续能验证它们被保留。
        state.store.agent.agent_config.trust_mode = TrustMode::Supervised;
        state.store.agent.agent_config.default_trust_mode = TrustMode::Supervised;
        state.store.agent.agent_config.custom_system_prompt = "runtime-prompt-marker".to_string();
        state.store.agent.agent_config.reasoning_effort = "high".to_string();

        state.persist_to_disk()?;

        let app_path = paths.fake_home.join(".tiangong").join("app.json");
        let mut app_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&app_path)?)?;

        // 2. 模拟 legacy 数据：手工注入 core 不再管理的 `mcp` 外部能力字段。
        app_json["agent_config"]["mcp"] = serde_json::json!({
            "servers": [{"name": "legacy-server", "transport": "stdio"}]
        });
        fs::write(&app_path, serde_json::to_string_pretty(&app_json)?)?;

        // 3. 加载 legacy app.json——serde 反序列化会静默忽略 `mcp` 未知字段，
        //    agent_config 内存中不含任何外部能力配置。
        let loaded = state
            .services
            .repository
            .load_from_disk()?
            .expect("应能从 legacy app.json 回读状态");
        let loaded_config = loaded.agent_config.as_ref().expect("应回读出 agent_config");

        // runtime 字段必须完整保留，不被 legacy mcp 注入影响。
        assert_eq!(loaded_config.trust_mode, TrustMode::Supervised);
        assert_eq!(loaded_config.custom_system_prompt, "runtime-prompt-marker");
        assert_eq!(loaded_config.reasoning_effort, "high");

        // 4. 把加载的配置应用回内存 state，再持久化（模拟正常 load → persist 周期）。
        state.store.agent.agent_config = loaded_config.clone();
        state.persist_to_disk()?;

        // 5. 回归断言：新 app.json 的 agent_config 不含 mcp，runtime 字段仍在。
        let repersisted: serde_json::Value = serde_json::from_str(&fs::read_to_string(&app_path)?)?;
        assert!(
            repersisted["agent_config"].get("mcp").is_none(),
            "core 持久化不得回写已脱离的外部能力字段 mcp"
        );
        assert_eq!(
            repersisted["agent_config"]["trust_mode"], "supervised",
            "runtime 字段 trust_mode 不得丢失"
        );
        assert_eq!(
            repersisted["agent_config"]["custom_system_prompt"], "runtime-prompt-marker",
            "runtime 字段 custom_system_prompt 不得丢失"
        );
        assert_eq!(
            repersisted["agent_config"]["reasoning_effort"], "high",
            "runtime 字段 reasoning_effort 不得丢失"
        );
        Ok(())
    })
}
