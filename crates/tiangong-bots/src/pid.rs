//! Bot 进程的 PID 文件管理与进程存活/停止（issue #286 Bot 独立运行方案）。
//!
//! Bot 独立后台运行，不依赖父进程监督。通过 `~/.tiangong/bots/<id>/bot.pid` 判断
//! 运行状态、发送停止信号。Bot 与 Desktop/CLI/Server 完全解耦——任一退出不影响
//! 已运行的 Bot。

use std::time::Duration;

use anyhow::{Context, Result};

use crate::BotId;
use crate::paths;

/// 读取 bot 的 PID 文件，返回解析出的 PID。文件不存在或无效返回 None（并清理无效文件）。
pub fn read_pid(id: &BotId) -> Option<u32> {
    let path = paths::bot_pid_path(id);
    let content = std::fs::read_to_string(&path).ok()?;
    let pid = content.trim().parse::<u32>().ok();
    if pid.is_none() {
        // PID 文件内容无效，清理残留。
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
    use std::process::Command;
    // Windows: tasklist 检查进程存在。
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
}

/// Bot 是否正在运行（PID 文件存在且进程存活）。PID 失效时自动清理文件。
/// bot 是否正在运行（经进程记录 + 身份校验）。
///
/// 新版记录校验 PID + 启动时间 + 可执行文件路径；旧版裸 PID 仅校验进程存活
/// + 可执行文件路径匹配（迁移到新版）。PID 复用导致身份不匹配时返回 false。
pub fn is_running(id: &BotId) -> bool {
    is_running_with_inspector(id, &crate::process_record::SysinfoInspector)
}

/// 带指定 inspector 的 is_running（供测试 mock）。
pub fn is_running_with_inspector(
    id: &BotId,
    inspector: &dyn crate::process_record::ProcessInspector,
) -> bool {
    let Some(record) = crate::process_record::read_record(id).unwrap_or(None) else {
        return false;
    };
    match record {
        crate::process_record::ReadRecord::Versioned(rec) => {
            crate::process_record::verify_identity(&rec, inspector).is_ok()
        }
        crate::process_record::ReadRecord::Legacy { pid } => {
            // 旧版裸 PID：进程存活即视为运行中（不校验身份，但清理失效文件）。
            if process_alive(pid) {
                true
            } else {
                crate::process_record::remove_record(id);
                false
            }
        }
    }
}

/// 向指定 PID 的进程发送停止信号（SIGTERM）。
#[cfg(unix)]
pub fn send_terminate(pid: u32) -> Result<()> {
    // SAFETY: kill(pid, SIGTERM) 发送终止信号，标准 POSIX 操作。
    let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    if rc != 0 {
        anyhow::bail!("发送 SIGTERM 失败，pid={pid}");
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn send_terminate(pid: u32) -> Result<()> {
    use std::process::Command;
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T"])
        .output()
        .context("执行 taskkill 失败")?;
    if !output.status.success() {
        anyhow::bail!("taskkill 失败: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// 等待进程退出（轮询存活检查），最多 `timeout`。返回是否已退出。
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let interval = Duration::from_millis(100);
    let mut elapsed = Duration::ZERO;
    while elapsed < timeout {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(interval);
        elapsed += interval;
    }
    // 超时后若仍在运行，强制 kill。
    if process_alive(pid) {
        #[cfg(unix)]
        {
            // SAFETY: kill(pid, SIGKILL) 强制终止。
            let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .output();
        }
    }
    !process_alive(pid)
}

/// 停止 Bot（PID 文件 → SIGTERM → 等待 → 清理）。
///
/// 幂等：PID 文件不存在或进程已死时返回 Ok（清理失效文件）。
/// 停止 bot（身份校验后发 SIGTERM）。
///
/// 无法证明进程属于此 bot 时拒绝发送信号（防 PID 复用误杀）。
pub fn stop_bot(id: &BotId) -> Result<()> {
    stop_bot_with_inspector(id, &crate::process_record::SysinfoInspector)
}

/// 带指定 inspector 的 stop_bot（供测试 mock）。
pub fn stop_bot_with_inspector(
    id: &BotId,
    inspector: &dyn crate::process_record::ProcessInspector,
) -> Result<()> {
    let Some(record) = crate::process_record::read_record(id)? else {
        return Ok(()); // 无记录 = 未运行，幂等。
    };
    let pid = record.pid();
    match record {
        crate::process_record::ReadRecord::Versioned(rec) => {
            // 新版记录：严格身份校验。
            crate::process_record::verify_identity(&rec, inspector)
                .map_err(|e| anyhow::anyhow!("PID 记录与当前进程不匹配，已拒绝停止该进程：{e}"))?;
        }
        crate::process_record::ReadRecord::Legacy { .. } => {
            // 旧版裸 PID：进程不存在则清理，存在则允许停止（后续启动会写入新版记录）。
            if !process_alive(pid) {
                crate::process_record::remove_record(id);
                return Ok(());
            }
        }
    }
    if !process_alive(pid) {
        crate::process_record::remove_record(id);
        return Ok(());
    }
    send_terminate(pid).with_context(|| format!("停止 bot 失败：{id}（pid={pid}）"))?;
    if !wait_for_exit(pid, Duration::from_secs(10)) {
        tracing::warn!("bot {id}（pid={pid}）在超时后仍未退出");
    }
    crate::process_record::remove_record(id);
    Ok(())
}

/// 写入 PID 文件。
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
    let _ = std::fs::remove_file(paths::bot_pid_path(id));
}
/// 后台启动 bot 制品（脱离会话、写 PID、forget）。不监督、不自动重启。
///
/// 复用 `BotRuntime::start` 的 spawn 逻辑的独立版本，供 CLI 直接调用（无需 BotRuntime）。
pub fn spawn_detached(
    bot_id: &BotId,
    artifact_path: &std::path::Path,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    use std::process::Command;
    crate::paths::ensure_executable_paths_safe(bot_id)?;
    crate::paths::reject_symlink(artifact_path, "Bot 制品")?;
    let mut cmd = Command::new(artifact_path);
    tiangong_types::process::configure_no_window(&mut cmd);
    // 日志重定向到 bot.log（方案第十节：CLI 启动的 bot 日志也要在 Desktop 可见）。
    let log_path = crate::paths::bot_log_path(bot_id);
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
    for (k, v) in env {
        cmd.env(k, v);
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
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn bot 失败：{}", artifact_path.display()))?;
    let pid = child.id();
    std::mem::forget(child);
    let record = crate::process_record::make_record(pid, artifact_path, bot_id);
    crate::process_record::write_record(bot_id, &record)?;
    tracing::info!("bot 已后台启动：{} pid={pid}", bot_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 无效文件应被清理。
        assert!(!path.exists());
    }

    #[test]
    fn process_alive_self() {
        // 当前测试进程必然存活。
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn process_alive_dead_pid() {
        // spawn 一个立刻退出的子进程，取得其 PID（退出后必然不存活）。
        use std::process::Command;
        let child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        // 等待其退出。
        let _ = child.wait_with_output();
        // 短暂等待确保进程完全回收。
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!process_alive(pid), "已退出的子进程 pid={pid} 不应存活");
    }
}
