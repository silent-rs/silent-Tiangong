//! 版本化的 bot 进程记录与身份校验（安全加固 2/4）。
//!
//! 替代裸 PID 文件，记录进程身份（PID + 启动时间 + 可执行文件路径），
//! 停止/状态查询前校验身份，防止 PID 复用导致误杀无关进程。
//!
//! 旧版裸数字 PID 文件兼容读取，但标记为 Legacy，需校验可执行文件路径后才能操作。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::BotId;
use crate::paths;

/// 进程记录版本。
const RECORD_VERSION: u32 = 1;

/// 带版本的 bot 进程记录（写入 bot.pid 文件）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessRecord {
    /// 记录格式版本。
    pub version: u32,
    /// 进程 PID。
    pub pid: u32,
    /// 操作系统报告的进程启动时间（Unix 秒）。
    pub started_at: u64,
    /// 规范化后的可执行文件路径。
    pub executable: String,
    /// Bot 实例 ID。
    pub bot_id: String,
}

/// 从操作系统获取的进程身份信息。
#[derive(Debug, Clone)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started_at: u64,
    pub executable: PathBuf,
}

/// 进程信息检查器抽象（支持 mock 测试）。
pub trait ProcessInspector: Send + Sync {
    /// 查询指定 PID 的进程身份。进程不存在返回 Ok(None)。
    fn inspect(&self, pid: u32) -> Result<Option<ProcessIdentity>>;
}

/// 基于 `sysinfo` 的进程检查器。
pub struct SysinfoInspector;

impl ProcessInspector for SysinfoInspector {
    fn inspect(&self, pid: u32) -> Result<Option<ProcessIdentity>> {
        use sysinfo::{Pid, System};
        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let Some(proc_info) = sys.process(Pid::from_u32(pid)) else {
            return Ok(None);
        };
        let executable = proc_info.exe().map(|p| p.to_path_buf()).unwrap_or_default();
        Ok(Some(ProcessIdentity {
            pid,
            started_at: proc_info.start_time(),
            executable,
        }))
    }
}

/// 读取 bot 的进程记录文件。
///
/// 支持新版 JSON 和旧版裸数字（标记 Legacy）。
pub fn read_record(id: &BotId) -> Result<Option<ReadRecord>> {
    let path = paths::bot_pid_path(id);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    // 尝试解析新版 JSON。
    if trimmed.starts_with('{') {
        match serde_json::from_str::<ProcessRecord>(trimmed) {
            Ok(record) => return Ok(Some(ReadRecord::Versioned(record))),
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                return Ok(None);
            }
        }
    }
    // 旧版裸数字。
    match trimmed.parse::<u32>() {
        Ok(pid) => Ok(Some(ReadRecord::Legacy { pid })),
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            Ok(None)
        }
    }
}

/// 读取到的进程记录（区分新版与旧版）。
#[derive(Debug)]
pub enum ReadRecord {
    /// 新版 JSON 记录。
    Versioned(ProcessRecord),
    /// 旧版裸数字 PID（需额外校验才能操作）。
    Legacy { pid: u32 },
}

impl ReadRecord {
    /// 获取记录中的 PID。
    pub fn pid(&self) -> u32 {
        match self {
            ReadRecord::Versioned(r) => r.pid,
            ReadRecord::Legacy { pid } => *pid,
        }
    }
}

/// 原子写入进程记录文件（临时文件 + rename）。
pub fn write_record(id: &BotId, record: &ProcessRecord) -> Result<()> {
    let path = paths::bot_pid_path(id);
    let json = serde_json::to_string_pretty(record).context("序列化进程记录失败")?;
    if let Err(error) = crate::store::atomic_write(&path, &json) {
        let _ = std::fs::remove_file(path.with_extension("tmp"));
        return Err(error);
    }
    Ok(())
}

/// 仅当记录仍指向给定 PID 时删除，避免清理掉后来启动的新进程记录。
pub fn remove_record_if_pid(id: &BotId, pid: u32) {
    let should_remove = read_record(id)
        .ok()
        .flatten()
        .is_some_and(|record| record.pid() == pid);
    if should_remove {
        remove_record(id);
    }
}

/// 清理一次失败写入可能留下的正式记录和临时文件。
pub(crate) fn cleanup_record_write(id: &BotId, pid: u32) {
    remove_record_if_pid(id, pid);
    let _ = std::fs::remove_file(paths::bot_pid_path(id).with_extension("tmp"));
}

/// 删除进程记录文件（若存在）。
pub fn remove_record(id: &BotId) {
    let _ = std::fs::remove_file(paths::bot_pid_path(id));
}

/// 校验进程记录与当前系统进程的身份是否匹配。
///
/// 比较项：PID 存活 + 启动时间 + 可执行文件路径。
/// 任意一项不匹配返回 Err（拒绝发送信号）。
pub fn verify_identity(record: &ProcessRecord, inspector: &dyn ProcessInspector) -> Result<()> {
    if record.version != RECORD_VERSION {
        return Err(anyhow!("不支持的进程记录版本：{}", record.version));
    }
    let identity = inspector
        .inspect(record.pid)?
        .ok_or_else(|| anyhow!("进程 {} 不存在", record.pid))?;
    if identity.started_at != record.started_at {
        return Err(anyhow!(
            "进程启动时间不匹配（记录 {}，实际 {}），可能为 PID 复用",
            record.started_at,
            identity.started_at
        ));
    }
    let record_executable = Path::new(&record.executable);
    if identity.executable.as_os_str().is_empty() || record_executable.as_os_str().is_empty() {
        return Err(anyhow!("进程可执行文件路径为空，无法确认进程身份"));
    }
    if identity.executable != record_executable {
        return Err(anyhow!(
            "可执行文件路径不匹配（记录 {}，实际 {}）",
            record.executable,
            identity.executable.display()
        ));
    }
    Ok(())
}

/// 校验操作系统报告的进程可执行文件与预期 Bot 制品相同。
pub fn verify_expected_executable(identity: &ProcessIdentity, expected: &Path) -> Result<()> {
    if identity.executable.as_os_str().is_empty() {
        return Err(anyhow!("进程可执行文件路径为空，无法确认进程身份"));
    }
    let expected = std::fs::canonicalize(expected)
        .with_context(|| format!("解析 Bot 制品路径失败：{}", expected.display()))?;
    let actual = std::fs::canonicalize(&identity.executable).with_context(|| {
        format!(
            "解析进程可执行文件路径失败：{}",
            identity.executable.display()
        )
    })?;
    if actual != expected {
        return Err(anyhow!(
            "可执行文件路径不匹配（期望 {}，实际 {}）",
            expected.display(),
            actual.display()
        ));
    }
    Ok(())
}

/// 从操作系统的实时身份信息创建进程记录。
pub fn record_for_process(
    pid: u32,
    expected_executable: &Path,
    bot_id: &BotId,
    inspector: &dyn ProcessInspector,
) -> Result<ProcessRecord> {
    let identity = inspector
        .inspect(pid)?
        .ok_or_else(|| anyhow!("进程 {pid} 不存在"))?;
    verify_expected_executable(&identity, expected_executable)?;
    Ok(ProcessRecord {
        version: RECORD_VERSION,
        pid: identity.pid,
        started_at: identity.started_at,
        executable: identity.executable.to_string_lossy().into_owned(),
        bot_id: bot_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock inspector 返回固定身份。
    struct MockInspector {
        identity: Option<ProcessIdentity>,
    }
    impl ProcessInspector for MockInspector {
        fn inspect(&self, _pid: u32) -> Result<Option<ProcessIdentity>> {
            Ok(self.identity.clone())
        }
    }

    #[test]
    fn verify_identity_matching() {
        let record = ProcessRecord {
            version: 1,
            pid: 12345,
            started_at: 1000,
            executable: "/usr/bin/bot".to_string(),
            bot_id: "testbot".to_string(),
        };
        let inspector = MockInspector {
            identity: Some(ProcessIdentity {
                pid: 12345,
                started_at: 1000,
                executable: PathBuf::from("/usr/bin/bot"),
            }),
        };
        assert!(verify_identity(&record, &inspector).is_ok());
    }

    #[test]
    fn verify_identity_start_time_mismatch() {
        let record = ProcessRecord {
            version: 1,
            pid: 12345,
            started_at: 1000,
            executable: "/usr/bin/bot".to_string(),
            bot_id: "testbot".to_string(),
        };
        let inspector = MockInspector {
            identity: Some(ProcessIdentity {
                pid: 12345,
                started_at: 2000,
                executable: PathBuf::from("/usr/bin/bot"),
            }),
        };
        assert!(verify_identity(&record, &inspector).is_err());
    }

    #[test]
    fn verify_identity_process_not_found() {
        let record = ProcessRecord {
            version: 1,
            pid: 12345,
            started_at: 1000,
            executable: "/usr/bin/bot".to_string(),
            bot_id: "testbot".to_string(),
        };
        let inspector = MockInspector { identity: None };
        assert!(verify_identity(&record, &inspector).is_err());
    }

    #[test]
    fn read_record_legacy_pid() {
        let id = BotId::try_from("legacytest").unwrap();
        let path = paths::bot_pid_path(&id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "12345").unwrap();
        match read_record(&id).unwrap() {
            Some(ReadRecord::Legacy { pid }) => assert_eq!(pid, 12345),
            other => panic!("expected Legacy, got {other:?}"),
        }
        remove_record(&id);
    }

    #[test]
    fn read_record_versioned_json() {
        let id = BotId::try_from("versiontest").unwrap();
        let path = paths::bot_pid_path(&id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let json = r#"{"version":1,"pid":999,"started_at":100,"executable":"/bot","bot_id":"x"}"#;
        std::fs::write(&path, json).unwrap();
        match read_record(&id).unwrap() {
            Some(ReadRecord::Versioned(record)) => {
                assert_eq!(record.pid, 999);
                assert_eq!(record.started_at, 100);
            }
            other => panic!("expected Versioned, got {other:?}"),
        }
        remove_record(&id);
    }

    #[test]
    fn read_record_missing_returns_none() {
        let id = BotId::try_from("missingtest").unwrap();
        assert!(read_record(&id).unwrap().is_none());
    }

    #[test]
    fn read_record_invalid_cleans_file() {
        let id = BotId::try_from("invalidtest").unwrap();
        let path = paths::bot_pid_path(&id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "garbage").unwrap();
        assert!(read_record(&id).unwrap().is_none());
        assert!(!path.exists(), "无效 PID 文件应被清理");
    }
}
