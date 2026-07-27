//! Bot 进程的 PID 文件管理与进程存活/停止（issue #286 Bot 独立运行方案）。
//!
//! Bot 独立后台运行，不依赖父进程监督。通过 `~/.tiangong/bots/<id>/bot.pid` 判断
//! 运行状态、发送停止信号。Bot 与 Desktop/CLI/Server 完全解耦——任一退出不影响
//! 已运行的 Bot。

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use crate::process_record::{
    ProcessInspector, ProcessRecord, ReadRecord, SysinfoInspector, record_for_process,
    verify_identity,
};
use crate::{BotId, paths};

/// 读取旧版裸 PID 文件。新版 JSON 记录不通过该兼容 API 暴露。
pub fn read_pid(id: &BotId) -> Option<u32> {
    let path = paths::bot_pid_path(id);
    let content = std::fs::read_to_string(&path).ok()?;
    let pid = content.trim().parse::<u32>().ok();
    if pid.is_none() && !content.trim().starts_with('{') {
        let _ = std::fs::remove_file(&path);
    }
    pid
}

/// 判断指定 PID 的进程是否存活。
#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) 是标准的进程存在性检查，信号 0 不实际发送信号。
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
pub fn process_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
}

/// Bot 是否正在运行（经进程记录 + 身份校验）。
///
/// 新版记录校验 PID + 启动时间 + 可执行文件路径。旧版裸 PID 只有在实时进程的
/// executable 与当前 Bot 制品完全匹配时才迁移成新版记录；无法确认身份时返回 false。
pub fn is_running(id: &BotId) -> bool {
    is_running_with_inspector(id, &SysinfoInspector)
}

/// 带指定 inspector 的 is_running（供测试 mock）。
pub fn is_running_with_inspector(id: &BotId, inspector: &dyn ProcessInspector) -> bool {
    let artifact = paths::bot_artifact_path(id);
    match resolve_record(id, &artifact, inspector) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(bot_id = %id, %error, "无法确认 bot 进程身份");
            false
        }
    }
}

/// 向指定 PID 的进程发送停止信号（SIGTERM）。
#[cfg(unix)]
pub fn send_terminate(pid: u32) -> Result<()> {
    // SAFETY: kill(pid, SIGTERM) 发送终止信号，标准 POSIX 操作。
    let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("发送 SIGTERM 失败，pid={pid}"));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn send_terminate(pid: u32) -> Result<()> {
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T"])
        .output()
        .context("执行 taskkill 失败")?;
    if !output.status.success() {
        anyhow::bail!("taskkill 失败: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// 停止 Bot（身份校验后发 SIGTERM）。
///
/// 幂等：进程记录不存在或记录中的原进程已经退出时返回 Ok。无法证明进程属于此
/// Bot 时拒绝发送信号，防止 PID 复用误杀。
pub fn stop_bot(id: &BotId) -> Result<()> {
    stop_bot_with_inspector(id, &SysinfoInspector)
}

/// 带指定 inspector 的 stop_bot（供测试 mock）。
pub fn stop_bot_with_inspector(id: &BotId, inspector: &dyn ProcessInspector) -> Result<()> {
    let artifact = paths::bot_artifact_path(id);
    let Some(record) = resolve_record(id, &artifact, inspector)? else {
        return Ok(());
    };

    // 在发信号前再次读取身份，缩小检查与操作之间的竞态窗口。
    verify_identity(&record, inspector)
        .map_err(|error| anyhow!("PID 记录与当前进程不匹配，已拒绝停止该进程：{error}"))?;
    send_terminate(record.pid)
        .with_context(|| format!("停止 bot 失败：{id}（pid={}）", record.pid))?;
    if !wait_for_record_exit(&record, inspector, Duration::from_secs(10))? {
        anyhow::bail!(
            "bot 在终止超时后仍然存活，已保留进程记录：{}（pid={}）",
            id,
            record.pid
        );
    }
    crate::process_record::remove_record_if_pid(id, record.pid);
    Ok(())
}

/// 写入旧版裸 PID 文件（仅保留给兼容测试和迁移工具）。
pub fn write_pid(id: &BotId, pid: u32) -> Result<()> {
    let path = paths::bot_pid_path(id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 PID 文件目录失败: {}", parent.display()))?;
    }
    std::fs::write(&path, pid.to_string())
        .with_context(|| format!("写入 PID 文件失败: {}", path.display()))?;
    Ok(())
}

/// 删除 PID 文件（若存在）。
pub fn remove_pid(id: &BotId) {
    crate::process_record::remove_record(id);
}

/// 后台启动 bot 制品（脱离会话、写身份记录、forget）。不监督、不自动重启。
pub fn spawn_detached(
    bot_id: &BotId,
    artifact_path: &Path,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    paths::ensure_executable_paths_safe(bot_id)?;
    paths::reject_symlink(artifact_path, "Bot 制品")?;
    let mut cmd = Command::new(artifact_path);
    tiangong_types::process::configure_no_window(&mut cmd);

    let log_path = paths::bot_log_path(bot_id);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stdout_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    let stderr_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    cmd.stdin(std::process::Stdio::null())
        .stdout(
            stdout_file
                .map(std::process::Stdio::from)
                .unwrap_or(std::process::Stdio::null()),
        )
        .stderr(
            stderr_file
                .map(std::process::Stdio::from)
                .unwrap_or(std::process::Stdio::null()),
        );
    for (key, value) in env {
        cmd.env(key, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                let _ = libc::setsid();
                Ok(())
            });
        }
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn bot 失败：{}", artifact_path.display()))?;
    std::thread::sleep(Duration::from_millis(500));
    complete_detached_start(
        &mut child,
        bot_id,
        artifact_path,
        |child| child.try_wait(),
        crate::process_record::write_record,
        &SysinfoInspector,
    )?;

    let pid = child.id();
    std::mem::forget(child);
    tracing::info!("bot 已后台启动：{} pid={}", bot_id, pid);
    Ok(())
}

fn resolve_record(
    id: &BotId,
    expected_artifact: &Path,
    inspector: &dyn ProcessInspector,
) -> Result<Option<ProcessRecord>> {
    let Some(record) = crate::process_record::read_record(id)? else {
        return Ok(None);
    };
    match record {
        ReadRecord::Versioned(record) => {
            if record.bot_id != id.as_str() {
                anyhow::bail!(
                    "进程记录 Bot ID 不匹配（记录 {}，期望 {}）",
                    record.bot_id,
                    id
                );
            }
            match verify_identity(&record, inspector) {
                Ok(()) => Ok(Some(record)),
                Err(error) => {
                    if inspector.inspect(record.pid)?.is_none() {
                        crate::process_record::remove_record_if_pid(id, record.pid);
                        Ok(None)
                    } else {
                        Err(error.context("PID 记录与当前进程身份不匹配"))
                    }
                }
            }
        }
        ReadRecord::Legacy { pid } => {
            let Some(_) = inspector.inspect(pid)? else {
                crate::process_record::remove_record_if_pid(id, pid);
                return Ok(None);
            };
            let record = record_for_process(pid, expected_artifact, id, inspector)
                .context("旧版 PID 无法确认属于当前 Bot，已拒绝操作")?;
            crate::process_record::write_record(id, &record)
                .context("迁移旧版 PID 记录失败，已拒绝操作")?;
            verify_identity(&record, inspector)
                .context("旧版 PID 迁移后身份发生变化，已拒绝操作")?;
            Ok(Some(record))
        }
    }
}

fn wait_for_record_exit(
    record: &ProcessRecord,
    inspector: &dyn ProcessInspector,
    timeout: Duration,
) -> Result<bool> {
    let interval = Duration::from_millis(100);
    let mut elapsed = Duration::ZERO;
    while elapsed < timeout {
        if !record_is_current(record, inspector)? {
            return Ok(true);
        }
        std::thread::sleep(interval);
        elapsed += interval;
    }

    // 只有原进程身份仍然匹配时才允许强制终止。PID 已复用视为原进程已退出。
    if !record_is_current(record, inspector)? {
        return Ok(true);
    }
    send_kill(record.pid)?;

    let mut elapsed = Duration::ZERO;
    while elapsed < Duration::from_secs(1) {
        if !record_is_current(record, inspector)? {
            return Ok(true);
        }
        std::thread::sleep(interval);
        elapsed += interval;
    }
    Ok(!record_is_current(record, inspector)?)
}

fn record_is_current(record: &ProcessRecord, inspector: &dyn ProcessInspector) -> Result<bool> {
    let Some(identity) = inspector.inspect(record.pid)? else {
        return Ok(false);
    };
    Ok(identity.started_at == record.started_at
        && !identity.executable.as_os_str().is_empty()
        && identity.executable == Path::new(&record.executable))
}

#[cfg(unix)]
fn send_kill(pid: u32) -> Result<()> {
    // SAFETY: 调用前已再次验证 PID、启动时间和可执行文件路径均匹配。
    let rc = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("发送 SIGKILL 失败，pid={pid}"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn send_kill(pid: u32) -> Result<()> {
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .context("执行 taskkill /F 失败")?;
    if !output.status.success() {
        anyhow::bail!(
            "taskkill /F 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn complete_detached_start<T, W>(
    child: &mut Child,
    bot_id: &BotId,
    artifact_path: &Path,
    try_wait: T,
    write_record: W,
    inspector: &dyn ProcessInspector,
) -> Result<()>
where
    T: FnOnce(&mut Child) -> std::io::Result<Option<ExitStatus>>,
    W: FnOnce(&BotId, &ProcessRecord) -> Result<()>,
{
    match try_wait(child) {
        Ok(Some(status)) => {
            let log_tail = crate::logger::read_log_tail(bot_id)
                .map(|log| log.content)
                .unwrap_or_default();
            let truncated: String = log_tail.chars().take(500).collect();
            return Err(anyhow!(
                "bot 启动后立即退出（{status}）。日志尾部：\n{truncated}"
            ));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(rollback_child(
                child,
                bot_id,
                anyhow!("检查 bot 启动状态失败：{error}"),
            ));
        }
    }

    let record = match record_for_process(child.id(), artifact_path, bot_id, inspector) {
        Ok(record) => record,
        Err(error) => {
            return Err(rollback_child(
                child,
                bot_id,
                error.context("读取 bot 子进程身份失败"),
            ));
        }
    };
    if let Err(error) = write_record(bot_id, &record) {
        return Err(rollback_child(
            child,
            bot_id,
            error.context("写入进程记录失败"),
        ));
    }
    Ok(())
}

fn rollback_child(child: &mut Child, bot_id: &BotId, error: anyhow::Error) -> anyhow::Error {
    let pid = child.id();
    let kill_error = child.kill().err();
    let wait_error = child.wait().err();
    crate::process_record::cleanup_record_write(bot_id, pid);

    let mut details = Vec::new();
    if let Some(error) = kill_error {
        details.push(format!("终止子进程失败：{error}"));
    }
    if let Some(error) = wait_error {
        details.push(format!("回收子进程失败：{error}"));
    }
    if details.is_empty() {
        error.context("已终止并回收 bot 子进程")
    } else {
        error.context(format!("bot 子进程回滚不完整：{}", details.join("；")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_record::ProcessIdentity;
    use std::path::PathBuf;

    struct MockInspector {
        identity: Option<ProcessIdentity>,
    }

    impl ProcessInspector for MockInspector {
        fn inspect(&self, _pid: u32) -> Result<Option<ProcessIdentity>> {
            Ok(self.identity.clone())
        }
    }

    #[test]
    fn read_pid_missing_returns_none() {
        let id = BotId::try_from("nonexistentpidtest").unwrap();
        assert_eq!(read_pid(&id), None);
    }

    #[test]
    fn write_and_read_pid_roundtrip() {
        let id = BotId::try_from("pidroundtriptest").unwrap();
        write_pid(&id, 12345).unwrap();
        assert_eq!(read_pid(&id), Some(12345));
        remove_pid(&id);
        assert_eq!(read_pid(&id), None);
    }

    #[test]
    fn read_pid_invalid_content_returns_none_and_cleans() {
        let id = BotId::try_from("pidinvalidtest").unwrap();
        let path = paths::bot_pid_path(&id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "not-a-pid").unwrap();
        assert_eq!(read_pid(&id), None);
        assert!(!path.exists());
    }

    #[test]
    fn process_alive_self() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn process_alive_dead_pid() {
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--help")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let _ = child.wait_with_output();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!process_alive(pid), "已退出的子进程 pid={pid} 不应存活");
    }

    #[test]
    fn legacy_pid_requires_matching_executable_before_migration() {
        let id = BotId::try_from("legacymismatch").unwrap();
        write_pid(&id, std::process::id()).unwrap();
        let inspector = MockInspector {
            identity: Some(ProcessIdentity {
                pid: std::process::id(),
                started_at: 1,
                executable: PathBuf::from("/definitely/not/the/bot"),
            }),
        };

        let error = resolve_record(&id, Path::new("/missing/bot"), &inspector).unwrap_err();
        assert!(error.to_string().contains("旧版 PID 无法确认"));
        assert!(process_alive(std::process::id()));
        remove_pid(&id);
    }

    #[test]
    fn matching_legacy_pid_migrates_to_versioned_record() {
        let id = BotId::try_from("legacymigration").unwrap();
        write_pid(&id, std::process::id()).unwrap();
        let executable = std::env::current_exe().unwrap();

        let record = resolve_record(&id, &executable, &SysinfoInspector)
            .unwrap()
            .expect("当前测试进程应可迁移");

        assert_eq!(record.pid, std::process::id());
        assert!(record.started_at > 0);
        assert_eq!(record.bot_id, id.as_str());
        match crate::process_record::read_record(&id).unwrap() {
            Some(ReadRecord::Versioned(saved)) => {
                assert_eq!(saved.pid, record.pid);
                assert_eq!(saved.started_at, record.started_at);
                assert_eq!(saved.executable, record.executable);
            }
            other => panic!("expected versioned record, got {other:?}"),
        }
        remove_pid(&id);
    }

    #[test]
    fn try_wait_error_rolls_back_child() {
        let id = BotId::try_from("trywaitrollback").unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut child = Command::new(&executable)
            .args(["--exact", "pid::tests::spawn_helper_sleeps", "--ignored"])
            .spawn()
            .unwrap();

        let result = complete_detached_start(
            &mut child,
            &id,
            &executable,
            |_| Err(std::io::Error::other("injected try_wait failure")),
            crate::process_record::write_record,
            &SysinfoInspector,
        );

        assert!(result.is_err());
        assert!(child.try_wait().unwrap().is_some());
        assert!(crate::process_record::read_record(&id).unwrap().is_none());
    }

    #[test]
    fn record_write_error_rolls_back_child() {
        let id = BotId::try_from("recordrollback").unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut child = Command::new(&executable)
            .args(["--exact", "pid::tests::spawn_helper_sleeps", "--ignored"])
            .spawn()
            .unwrap();

        let result = complete_detached_start(
            &mut child,
            &id,
            &executable,
            |_| Ok(None),
            |_, _| Err(anyhow!("injected write failure")),
            &SysinfoInspector,
        );

        assert!(result.is_err());
        assert!(child.try_wait().unwrap().is_some());
        assert!(crate::process_record::read_record(&id).unwrap().is_none());
    }

    #[test]
    fn immediate_exit_returns_error_without_record() {
        let id = BotId::try_from("immediateexit").unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut child = Command::new(&executable)
            .args([
                "--exact",
                "pid::tests::spawn_helper_exits_immediately",
                "--ignored",
            ])
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));

        let result = complete_detached_start(
            &mut child,
            &id,
            &executable,
            |child| child.try_wait(),
            crate::process_record::write_record,
            &SysinfoInspector,
        );

        assert!(result.unwrap_err().to_string().contains("立即退出"));
        assert!(crate::process_record::read_record(&id).unwrap().is_none());
    }

    #[test]
    #[ignore]
    fn spawn_helper_exits_immediately() {}

    #[test]
    #[ignore]
    fn spawn_helper_sleeps() {
        std::thread::sleep(Duration::from_secs(30));
    }
}
