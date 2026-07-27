//! bot 进程监督——spawn 子进程、stdout/stderr 写入日志文件、崩溃自动重启。
//!
//! bot 的 stdout/stderr 合并写入 `~/.tiangong/bots/<id>/bot.log`（每行标注
//! 来源与时间），内存只保留最近 8KB 的 stderr 错误摘要供健康状态展示。
//! 详见 [`crate::logger`]。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::logger::BotLogger;
use crate::{BotId, paths};

/// 单次崩溃重启后等待的最小/最大退避。
const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// 被监督的 bot 进程句柄。
pub struct SupervisedBot {
    /// 停止信号发送端（drop 即触发优雅停止）。
    stop_tx: Option<oneshot::Sender<()>>,
    /// 监督循环 task。
    handle: Option<JoinHandle<()>>,
    /// 日志写入器（供外部读取错误摘要）。
    logger: Arc<BotLogger>,
}

use std::sync::Arc;

impl SupervisedBot {
    /// 取最近的错误摘要（stderr，供健康状态展示）。
    pub async fn error_summary(&self) -> String {
        self.logger.error_summary().await
    }

    /// 请求停止并等待监督循环退出。
    pub async fn stop(mut self) -> Result<()> {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            handle.await.context("等待 bot 监督任务停止失败")?;
        }
        Ok(())
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
/// `bot_id` 用于确定日志路径（`~/.tiangong/bots/<bot_id>/bot.log`）。
/// `artifact_path` 是制品可执行文件路径；`env` 是注入的环境变量（凭证等）。
pub(crate) fn spawn_supervised(
    bot_id: &BotId,
    artifact_path: PathBuf,
    env: BTreeMap<String, String>,
) -> Result<SupervisedBot> {
    paths::ensure_executable_paths_safe(bot_id)?;
    if !artifact_path.exists() {
        return Err(anyhow::anyhow!(
            "bot 制品不存在：{}",
            artifact_path.display()
        ));
    }

    let logger = Arc::new(BotLogger::new(paths::bot_log_path(bot_id)));
    let (stop_tx, stop_rx) = oneshot::channel::<()>();

    let bot_id_for_task = bot_id.clone();
    let logger_for_task = logger.clone();
    let handle = tokio::spawn(supervise_loop(
        bot_id_for_task,
        artifact_path,
        env,
        logger_for_task,
        stop_rx,
    ));

    Ok(SupervisedBot {
        stop_tx: Some(stop_tx),
        handle: Some(handle),
        logger,
    })
}

/// 监督循环：spawn → 等待退出 → 崩溃则退避后重启，直到收到停止信号。
async fn supervise_loop(
    bot_id: BotId,
    artifact_path: PathBuf,
    env: BTreeMap<String, String>,
    logger: Arc<BotLogger>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let mut backoff = MIN_BACKOFF;
    let mut last_pid: Option<u32> = None;
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

        let (registered_child, pid) = match register_child(
            child,
            &bot_id,
            &artifact_path,
            &crate::process_record::SysinfoInspector,
            crate::process_record::write_record,
        )
        .await
        {
            Ok(registered) => registered,
            Err(error) => {
                tracing::error!(
                    "启动 bot 后写入进程记录失败（{}）：{error}",
                    artifact_path.display()
                );
                if wait_or_stop(&mut stop_rx, backoff).await {
                    break;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };
        child = registered_child;
        last_pid = Some(pid);

        // 立即 take stdout/stderr 交给 BotLogger 消费——必须在 wait() 之前，
        // 否则子进程写满管道缓冲区（默认 64KB）后会阻塞，导致 wait() 死锁。
        use crate::logger::StreamKind;
        let stdout_task = logger
            .clone()
            .consume_stream(child.stdout.take(), StreamKind::Stdout);
        let stderr_task = logger
            .clone()
            .consume_stream(child.stderr.take(), StreamKind::Stderr);

        // 等待子进程退出或停止信号。
        tokio::select! {
            status = child.wait() => {
                // 等管道消费 task 读完。
                let _ = stdout_task.await;
                let _ = stderr_task.await;
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
                let _ = stdout_task.await;
                let _ = stderr_task.await;
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

    // 条件清理进程记录：仅当记录中 PID == 本轮最后 PID 时才删除。
    if let Some(my_pid) = last_pid {
        crate::process_record::remove_record_if_pid(&bot_id, my_pid);
    }
}

/// spawn 单个 bot 子进程，stdout/stderr pipe 用于日志捕获。
fn spawn_child(artifact_path: &PathBuf, env: &BTreeMap<String, String>) -> Result<Child> {
    if let Some(runtime_dir) = artifact_path.parent() {
        paths::reject_symlink(runtime_dir, "Bot 实例目录")?;
    }
    paths::reject_symlink(artifact_path, "Bot 制品")?;
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

async fn register_child<W>(
    mut child: Child,
    bot_id: &BotId,
    artifact_path: &std::path::Path,
    inspector: &dyn crate::process_record::ProcessInspector,
    write_record: W,
) -> Result<(Child, u32)>
where
    W: FnOnce(&BotId, &crate::process_record::ProcessRecord) -> Result<()>,
{
    let pid = child.id().context("spawn 成功但未返回 bot PID")?;
    let result = crate::process_record::record_for_process(pid, artifact_path, bot_id, inspector)
        .and_then(|record| write_record(bot_id, &record));
    if let Err(error) = result {
        let kill_error = child.kill().await.err();
        let wait_error = child.wait().await.err();
        crate::process_record::cleanup_record_write(bot_id, pid);

        let mut details = Vec::new();
        if let Some(error) = kill_error {
            details.push(format!("终止子进程失败：{error}"));
        }
        if let Some(error) = wait_error {
            details.push(format!("回收子进程失败：{error}"));
        }
        return if details.is_empty() {
            Err(error.context("已终止并回收 bot 子进程"))
        } else {
            Err(error.context(format!("bot 子进程回滚不完整：{}", details.join("；"))))
        };
    }
    Ok((child, pid))
}

/// 等待 `delay` 或停止信号；返回 true 表示收到停止信号。
async fn wait_or_stop(stop_rx: &mut oneshot::Receiver<()>, delay: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        _ = stop_rx => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_write_error_terminates_supervised_child() {
        let id = BotId::try_from("supervisorrollback").unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(&executable);
        command
            .args([
                "--exact",
                "supervisor::tests::spawn_helper_sleeps",
                "--ignored",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command.spawn().unwrap();
        let pid = child.id().unwrap();

        let result = register_child(
            child,
            &id,
            &executable,
            &crate::process_record::SysinfoInspector,
            |_, _| Err(anyhow::anyhow!("injected write failure")),
        )
        .await;

        assert!(result.is_err());
        assert!(!crate::pid::process_alive(pid));
        assert!(crate::process_record::read_record(&id).unwrap().is_none());
    }

    #[test]
    #[ignore]
    fn spawn_helper_sleeps() {
        std::thread::sleep(Duration::from_secs(30));
    }
}
