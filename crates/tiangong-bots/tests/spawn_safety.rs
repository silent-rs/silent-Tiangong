//! Bot 进程记录与启动环境的公开 API 集成测试。
//!
//! 进程启动的注入式异常路径由 `pid` / `supervisor` 模块单测覆盖；这里验证公开的
//! 身份校验、版本化记录读取和启动环境入口。

use std::collections::BTreeMap;

use tiangong_bots::pid::{is_running_with_inspector, stop_bot_with_inspector};
use tiangong_bots::{
    BotId, build_launch_env,
    process_record::{
        ProcessIdentity, ProcessInspector, ProcessRecord, ReadRecord, SysinfoInspector,
        read_record, verify_identity, write_record,
    },
};

struct MockInspector {
    identity: Option<ProcessIdentity>,
}

impl ProcessInspector for MockInspector {
    fn inspect(&self, _pid: u32) -> anyhow::Result<Option<ProcessIdentity>> {
        Ok(self.identity.clone())
    }
}

#[test]
fn stop_bot_rejects_identity_mismatch() {
    let id = BotId::try_from("identitymismatch").unwrap();
    let record = ProcessRecord {
        version: 1,
        pid: std::process::id(),
        started_at: 1,
        executable: "/fake/path".to_string(),
        bot_id: id.to_string(),
    };
    write_record(&id, &record).unwrap();

    let result = stop_bot_with_inspector(&id, &SysinfoInspector);
    assert!(result.is_err(), "身份不匹配时应拒绝停止，实际: {result:?}");
    let error = result.unwrap_err().to_string();
    assert!(error.contains("不匹配"), "实际错误: {error}");
    assert!(tiangong_bots::pid::process_alive(std::process::id()));

    tiangong_bots::process_record::remove_record(&id);
}

#[test]
fn legacy_pid_compat_read() {
    let id = BotId::try_from("legacypidcompat").unwrap();
    let path = tiangong_bots::paths::bot_pid_path(&id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, "99999").unwrap();

    match read_record(&id).unwrap() {
        Some(ReadRecord::Legacy { pid }) => assert_eq!(pid, 99999),
        other => panic!("expected Legacy, got {other:?}"),
    }
    tiangong_bots::process_record::remove_record(&id);
}

#[test]
fn write_and_read_versioned_record() {
    let id = BotId::try_from("writeversioned").unwrap();
    let record = ProcessRecord {
        version: 1,
        pid: 12345,
        started_at: 100,
        executable: "/test/bot".to_string(),
        bot_id: id.to_string(),
    };
    write_record(&id, &record).unwrap();

    match read_record(&id).unwrap() {
        Some(ReadRecord::Versioned(read_back)) => {
            assert_eq!(read_back.pid, 12345);
            assert_eq!(read_back.started_at, 100);
            assert_eq!(read_back.executable, "/test/bot");
            assert_eq!(read_back.bot_id, id.to_string());
        }
        other => panic!("expected Versioned, got {other:?}"),
    }
    tiangong_bots::process_record::remove_record(&id);
}

#[test]
fn verify_identity_process_not_found() {
    let record = ProcessRecord {
        version: 1,
        pid: 999999,
        started_at: 0,
        executable: "/test".to_string(),
        bot_id: "ghost".to_string(),
    };
    let inspector = MockInspector { identity: None };
    assert!(verify_identity(&record, &inspector).is_err());
}

#[test]
fn is_running_no_record() {
    let id = BotId::try_from("norunningrecord").unwrap();
    let inspector = MockInspector { identity: None };
    assert!(!is_running_with_inspector(&id, &inspector));
}

#[test]
fn build_launch_env_without_schema_returns_host_env() {
    let id = BotId::try_from("missingrequiredenv").unwrap();
    let bot = tiangong_bots::config::BotConfig {
        id,
        artifact_id: "test".to_string(),
        enabled: true,
        config: Default::default(),
        created_at: "2024-01-01".to_string(),
        updated_at: "2024-01-01".to_string(),
    };
    let mut extra = BTreeMap::new();
    extra.insert("TIANGONG_URL".to_string(), "http://127.0.0.1".to_string());

    let env = build_launch_env(&bot, &extra).unwrap();
    assert_eq!(env, extra);
}
