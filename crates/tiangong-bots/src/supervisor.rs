//! bot 进程监督——spawn 子进程、捕获 stderr tail、崩溃自动重启。
//!
//! 借鉴 `tiangong-plugin-mcp/src/client.rs:452-474` 的 stderr 捕获模式与
//! `tiangong-server/src/daemon.rs` 的 PID 管理思路。bot 子进程的凭证通过
//! 环境变量注入（见各 bot 制品文档，飞书用 `TIANGONG_BOT_FEISHU_*`）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

/// stderr tail 缓冲上限（字节），对齐 MCP client 的 8KB。
const STDERR_TAIL_BYTES: usize = 8 * 1024;

/// 单次崩溃重启后等待的最小/最大退避。
const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// 被监督的 bot 进程句柄。
pub struct SupervisedBot {
    /// 停止信号发送端（drop 即触发优雅停止）。
    stop_tx: Option<oneshot::Sender<()>>,
    /// 监督循环 task。
    handle: Option<JoinHandle<()>>,
    /// stderr tail（最近 STDERR_TAIL_BYTES 字节）。
    stderr_tail: Arc<Mutex<String>>,
}

impl SupervisedBot {
    /// 取最近的 stderr tail（诊断用）。
    pub async fn stderr_tail(&self) -> String {
        self.stderr_tail.lock().await.clone()
    }

    /// 请求停止并等待监督循环退出。
    pub async fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }

    /// 监督任务是否已经结束（bot 退出且未重启，或被 stop）。
    ///
    /// 用于运行表判断 bot 是否仍实际运行——join handle 完成意味着
    /// supervise_loop 已 break，bot 不再运行。
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(true)
    }
}

/// spawn 一个 bot 子进程并启动监督循环（崩溃自动重启）。
///
/// `artifact_path` 是制品可执行文件路径；`env` 是注入的环境变量（凭证等）。
pub fn spawn_supervised(
    artifact_path: PathBuf,
    env: BTreeMap<String, String>,
) -> Result<SupervisedBot> {
    if !artifact_path.exists() {
        return Err(anyhow::anyhow!(
            "bot 制品不存在：{}",
            artifact_path.display()
        ));
    }

    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let (stop_tx, stop_rx) = oneshot::channel::<()>();

    let tail_for_task = stderr_tail.clone();
    let handle = tokio::spawn(supervise_loop(artifact_path, env, tail_for_task, stop_rx));

    Ok(SupervisedBot {
        stop_tx: Some(stop_tx),
        handle: Some(handle),
        stderr_tail,
    })
}

/// 监督循环：spawn → 等待退出 → 崩溃则退避后重启，直到收到停止信号。
async fn supervise_loop(
    artifact_path: PathBuf,
    env: BTreeMap<String, String>,
    stderr_tail: Arc<Mutex<String>>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let mut backoff = MIN_BACKOFF;
    loop {
        // 停止信号优先。
        if stop_rx.try_recv().is_ok() {
            break;
        }

        let mut child = match spawn_child(&artifact_path, &env) {
            Ok(child) => child,
            Err(err) => {
                tracing::error!("启动 bot 失败（{}）：{err}", artifact_path.display());
                if wait_or_stop(&mut stop_rx, backoff).await {
                    break;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let pid = child.id();
        tracing::info!("bot 已启动：{} pid={pid:?}", artifact_path.display());

        // 写 PID 文件。
        if let Some(pid) = pid {
            let pid_path = artifact_pid_path(&artifact_path);
            if let Some(parent) = pid_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&pid_path, pid.to_string());
        }

        // 立即 take stdout/stderr 并 spawn 持续消费 task——必须在 wait() 之前，
        // 否则子进程写满管道缓冲区（默认 64KB）后会阻塞，导致 wait() 死锁。
        let stdout_task = spawn_stream_drain(child.stdout.take());
        let stderr_task = spawn_stream_tail(child.stderr.take(), stderr_tail.clone());

        // 等待子进程退出或停止信号。
        tokio::select! {
            status = child.wait() => {
                // 等管道 drain task 读完后取最终 tail。
                let _ = stdout_task.await;
                let final_tail = stderr_task.await.unwrap_or_default();
                if !final_tail.is_empty() {
                    *stderr_tail.lock().await = final_tail;
                }
                match status {
                    Ok(s) if s.success() => {
                        tracing::info!("bot 正常退出：{}", artifact_path.display());
                        break;
                    }
                    Ok(s) => {
                        tracing::warn!("bot 异常退出：{} {s}", artifact_path.display());
                    }
                    Err(err) => {
                        tracing::warn!("bot 等待失败：{} {err}", artifact_path.display());
                    }
                }
            }
            _ = &mut stop_rx => {
                tracing::info!("收到停止信号，终止 bot：{}", artifact_path.display());
                let _ = child.kill().await;
                let _ = child.wait().await;
                // 取最终 tail 用于诊断。
                let final_tail = stderr_task.await.unwrap_or_default();
                if !final_tail.is_empty() {
                    *stderr_tail.lock().await = final_tail;
                }
                break;
            }
        }

        // 崩溃退避。
        tracing::info!("{} 后重启 bot", backoff.as_secs());
        if wait_or_stop(&mut stop_rx, backoff).await {
            break;
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }

    // 清理 PID 文件。
    let _ = std::fs::remove_file(artifact_pid_path(&artifact_path));
}

/// spawn 单个 bot 子进程，stderr pipe 用于 tail 捕获。
fn spawn_child(artifact_path: &PathBuf, env: &BTreeMap<String, String>) -> Result<Child> {
    let mut cmd = Command::new(artifact_path);
    tiangong_types::process::configure_tokio_no_window(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.kill_on_drop(true);
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn bot 失败：{}", artifact_path.display()))?;
    Ok(child)
}

/// 持续读取 stdout 并丢弃（仅消费流，防止管道写满阻塞子进程）。
///
/// 返回 JoinHandle，读完后（EOF/错误）task 结束。
fn spawn_stream_drain(stream: Option<tokio::process::ChildStdout>) -> tokio::task::JoinHandle<()> {
    let Some(stream) = stream else {
        return tokio::spawn(async {});
    };
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut reader = stream;
        let mut buf = vec![0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
}

/// 持续读取 stderr 并保留最后 STDERR_TAIL_BYTES 字节到 tail 缓冲。
///
/// 返回 JoinHandle，读完后返回最终 tail 文本。
fn spawn_stream_tail(
    stream: Option<tokio::process::ChildStderr>,
    tail: Arc<Mutex<String>>,
) -> tokio::task::JoinHandle<String> {
    let Some(stream) = stream else {
        return tokio::spawn(async { String::new() });
    };
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut reader = stream;
        let mut buf = vec![0u8; 4096];
        let mut collected: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    collected.extend_from_slice(&buf[..n]);
                    // 实时更新 tail，避免只在结束时才写入。
                    let snap = if collected.len() > STDERR_TAIL_BYTES {
                        String::from_utf8_lossy(&collected[collected.len() - STDERR_TAIL_BYTES..])
                            .to_string()
                    } else {
                        String::from_utf8_lossy(&collected).to_string()
                    };
                    *tail.lock().await = snap;
                }
            }
        }
        // 最终 tail。
        if collected.len() > STDERR_TAIL_BYTES {
            collected = collected.split_off(collected.len() - STDERR_TAIL_BYTES);
        }
        String::from_utf8_lossy(&collected).to_string()
    })
}

/// 等待 `delay` 或停止信号；返回 true 表示收到停止信号。
async fn wait_or_stop(stop_rx: &mut oneshot::Receiver<()>, delay: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        _ = stop_rx => true,
    }
}

/// 由制品路径推导 PID 文件路径（同目录下 `bot.pid`）。
fn artifact_pid_path(artifact_path: &std::path::Path) -> PathBuf {
    artifact_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("bot.pid")
}
