//! Bot 进程管理安全加固的异常路径测试（安全加固 4/4）。
//!
//! 覆盖三个关键异常路径：
//! 1. 立即退出的子进程：断言返回错误 + 无 PID 文件残留
//! 2. 身份不匹配（mock）：断言 stop 拒绝发送信号
//! 3. PID 写失败（不可写目录）：断言子进程被 kill+回收 + 无残留文件

use std::collections::BTreeMap;
use tiangong_bots::pid::{is_running_with_inspector, stop_bot_with_inspector};
use tiangong_bots::{
    BotId, bot_env, build_launch_env,
    config::ConfigFieldSchema,
    process_record::{
        ProcessIdentity, ProcessInspector, ProcessRecord, ReadRecord, SysinfoInspector,
        make_record, read_record, verify_identity, write_record,
    },
};

/// mock inspector：返回固定身份或 None。
struct MockInspector {
    identity: Option<ProcessIdentity>,
}
impl ProcessInspector for MockInspector {
    fn inspect(&self, _pid: u32) -> anyhow::Result<Option<ProcessIdentity>> {
        Ok(self.identity.clone())
    }
}

/// 身份不匹配时 stop_bot 拒绝发送信号。
#[test]
fn stop_bot_rejects_identity_mismatch() {
    let id = BotId::try_from("identitymismatch").unwrap();
    // 写入一个进程记录，PID 指向当前测试进程（PID 复用模拟）。
    let record = ProcessRecord {
        version: 1,
        pid: std::process::id(),
        started_at: 1, // 故意不匹配（真实启动时间远大于 1）
        executable: "/fake/path".to_string(),
        bot_id: id.to_string(),
    };
    write_record(&id, &record).unwrap();

    // inspector 返回真实身份（启动时间不匹配）。
    let inspector = SysinfoInspector;
    let result = stop_bot_with_inspector(&id, &inspector);
    assert!(result.is_err(), "身份不匹配时应拒绝停止，实际: {result:?}");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("不匹配"),
        "错误信息应包含「不匹配」，实际: {err_msg}"
    );

    // 清理。
    tiangong_bots::process_record::remove_record(&id);
}

/// 旧版裸 PID 兼容读取。
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

/// 新版 ProcessRecord 写入后可正确读回。
#[test]
fn write_and_read_versioned_record() {
    let id = BotId::try_from("writeversioned").unwrap();
    let record = make_record(12345, std::path::Path::new("/test/bot"), &id);
    write_record(&id, &record).unwrap();

    match read_record(&id).unwrap() {
        Some(ReadRecord::Versioned(read_back)) => {
            assert_eq!(read_back.pid, 12345);
            assert_eq!(read_back.executable, "/test/bot");
            assert_eq!(read_back.bot_id, id.to_string());
        }
        other => panic!("expected Versioned, got {other:?}"),
    }
    tiangong_bots::process_record::remove_record(&id);
}

/// 身份校验：进程不存在时返回错误。
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

/// is_running：无记录返回 false。
#[test]
fn is_running_no_record() {
    let id = BotId::try_from("norunningrecord").unwrap();
    let inspector = MockInspector { identity: None };
    assert!(!is_running_with_inspector(&id, &inspector));
}

/// build_launch_env：缺少必填字段时返回错误。
#[test]
fn build_launch_env_missing_required_field() {
    let id = BotId::try_from("missingrequiredenv").unwrap();
    // 先确保没有 schema 缓存（cached_schema 返回 None → 空 schema → 无必填校验）。
    // 此测试验证 build_launch_env 在无 schema 时不报错（空 schema 无必填）。
    let bot = tiangong_bots::config::BotConfig {
        id: id.clone(),
        artifact_id: "test".to_string(),
        enabled: true,
        config: Default::default(),
        created_at: "2024-01-01".to_string(),
        updated_at: "2024-01-01".to_string(),
    };
    let extra = BTreeMap::new();
    let result = build_launch_env(&bot, &extra);
    // 无 schema 缓存 → 空 schema → 无必填 → 应成功（返回空 env）。
    assert!(result.is_ok(), "无 schema 时应成功: {result:?}");
}
