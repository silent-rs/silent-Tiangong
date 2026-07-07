//! 后台任务注册表与执行实现。
//!
//! 原 `tiangong-core::tool::background_task`，随 task 插件化迁出（#208）。
//! `task_registry()` 保持全局 `OnceLock` 静态，确保 GUI 管理（src-tauri）与
//! LLM 工具调用（`spawn_task` 等）命中同一注册表。

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use tiangong_types::process::configure_no_window;

/// 全局后台任务注册表
static TASK_REGISTRY: OnceLock<Arc<Mutex<TaskRegistry>>> = OnceLock::new();

pub fn task_registry() -> Arc<Mutex<TaskRegistry>> {
    TASK_REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(TaskRegistry::default())))
        .clone()
}

#[derive(Debug, Default)]
pub struct TaskRegistry {
    tasks: HashMap<String, BackgroundTask>,
}

#[derive(Debug)]
struct BackgroundTask {
    id: String,
    name: String,
    command: String,
    started_at: Instant,
    child: Option<Child>,
    status: TaskStatus,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskStatus {
    Running,
    Completed { exit_code: i32 },
    Failed { error: String },
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub name: String,
    pub command: String,
    pub status: TaskStatus,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

impl TaskRegistry {
    /// 启动后台任务。
    ///
    /// **输出缓冲限制**：stdout/stderr 设为 piped，但仅在任务结束后通过
    /// `wait_with_output()` 一次性收集。若子进程持续输出填满 OS pipe 缓冲区
    /// （通常 64KB），写端会阻塞，任务将无法结束。对高输出/长期运行的任务
    /// （dev server、日志刷屏等），建议在 cmd/args 中将输出重定向到文件。
    pub fn spawn(
        &mut self,
        name: String,
        cmd: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
    ) -> Result<String, String> {
        let id = scru128::new().to_string();

        let mut command = Command::new(&cmd);
        configure_no_window(&mut command);
        command
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(dir) = &cwd {
            command.current_dir(dir);
        }
        for (k, v) in &env {
            command.env(k, v);
        }

        let child = command
            .spawn()
            .map_err(|e| format!("启动后台任务失败：{e}"))?;

        let cmd_display = if args.is_empty() {
            cmd.clone()
        } else {
            format!("{} {}", cmd, args.join(" "))
        };

        self.tasks.insert(
            id.clone(),
            BackgroundTask {
                id: id.clone(),
                name,
                command: cmd_display,
                started_at: Instant::now(),
                child: Some(child),
                status: TaskStatus::Running,
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        Ok(id)
    }

    /// 查询任务状态（自动收集已完成任务的输出）
    pub fn query(&mut self, task_id: &str) -> Option<TaskInfo> {
        let task = self.tasks.get_mut(task_id)?;

        // 如果还在运行，检查是否已完成
        if matches!(task.status, TaskStatus::Running)
            && let Some(ref mut child) = task.child
        {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // 收集输出
                    let child = task.child.take().unwrap();
                    if let Ok(output) = child.wait_with_output() {
                        task.stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        task.stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    }
                    task.status = TaskStatus::Completed {
                        exit_code: status.code().unwrap_or(-1),
                    };
                }
                Ok(None) => {} // 还在运行
                Err(e) => {
                    task.status = TaskStatus::Failed {
                        error: e.to_string(),
                    };
                }
            }
        }

        Some(TaskInfo {
            id: task.id.clone(),
            name: task.name.clone(),
            command: task.command.clone(),
            status: task.status.clone(),
            duration_ms: task.started_at.elapsed().as_millis() as u64,
            stdout: task.stdout.clone(),
            stderr: task.stderr.clone(),
        })
    }

    /// 列出所有任务
    pub fn list(&mut self) -> Vec<TaskInfo> {
        let ids: Vec<String> = self.tasks.keys().cloned().collect();
        ids.iter().filter_map(|id| self.query(id)).collect()
    }

    /// 取消任务
    pub fn cancel(&mut self, task_id: &str) -> Option<TaskInfo> {
        let task = self.tasks.get_mut(task_id)?;
        if let Some(ref mut child) = task.child {
            let _ = child.kill();
            let child = task.child.take().unwrap();
            if let Ok(output) = child.wait_with_output() {
                task.stdout = String::from_utf8_lossy(&output.stdout).to_string();
                task.stderr = String::from_utf8_lossy(&output.stderr).to_string();
            }
        }
        task.status = TaskStatus::Cancelled;

        Some(TaskInfo {
            id: task.id.clone(),
            name: task.name.clone(),
            command: task.command.clone(),
            status: task.status.clone(),
            duration_ms: task.started_at.elapsed().as_millis() as u64,
            stdout: task.stdout.clone(),
            stderr: task.stderr.clone(),
        })
    }

    /// 收集指定任务的最新状态（内部方法，不返回 TaskInfo）
    fn refresh_task_status(&mut self, task_id: &str) {
        let Some(task) = self.tasks.get_mut(task_id) else {
            return;
        };
        if !matches!(task.status, TaskStatus::Running) {
            return;
        }
        if let Some(ref mut child) = task.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let child = task.child.take().unwrap();
                    if let Ok(output) = child.wait_with_output() {
                        task.stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        task.stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    }
                    task.status = TaskStatus::Completed {
                        exit_code: status.code().unwrap_or(-1),
                    };
                }
                Ok(None) => {}
                Err(e) => {
                    task.status = TaskStatus::Failed {
                        error: e.to_string(),
                    };
                }
            }
        }
    }

    /// 检查指定的任务是否全部完成（非 Running）。
    ///
    /// 仅判断 Running 状态，不区分「不存在」——缺失 id 由调用方（`wait_tasks`）
    /// 在轮询前单独检测，避免把「不存在」混入「仍在运行」语义导致无限等待。
    fn all_finished(&mut self, task_ids: &[String]) -> bool {
        for id in task_ids {
            self.refresh_task_status(id);
            if let Some(task) = self.tasks.get(id)
                && matches!(task.status, TaskStatus::Running)
            {
                return false;
            }
        }
        true
    }
}

/// 等待多个后台任务完成（spawn+join 模式）
///
/// 在持有锁外轮询，避免长时间阻塞其他操作。
/// `timeout_ms` 为 0 表示无限等待（仅对真实存在的 Running 任务；不存在的 task id
/// 会立即返回，不阻塞）。
pub fn wait_tasks(task_ids: Vec<String>, timeout_ms: u64) -> Vec<TaskInfo> {
    let registry = task_registry();
    let deadline = if timeout_ms > 0 {
        Some(Instant::now() + Duration::from_millis(timeout_ms))
    } else {
        None
    };

    loop {
        // 短暂持锁检查状态
        {
            let mut reg = match registry.lock() {
                Ok(reg) => reg,
                Err(_) => break,
            };
            // 先检测缺失：任一 task id 不存在立即返回（已完成的 + 缺失的被过滤），
            // 由 handle_wait 据 results.len() < requested 报错，避免无限等待。
            if task_ids.iter().any(|id| !reg.tasks.contains_key(id)) {
                return task_ids.iter().filter_map(|id| reg.query(id)).collect();
            }
            if reg.all_finished(&task_ids) {
                // 全部完成，收集结果
                return task_ids.iter().filter_map(|id| reg.query(id)).collect();
            }
        }

        // 检查超时
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            // 超时，返回当前状态
            let mut reg = match registry.lock() {
                Ok(reg) => reg,
                Err(_) => break,
            };
            return task_ids.iter().filter_map(|id| reg.query(id)).collect();
        }

        // 释放锁后短暂休眠，避免忙等
        std::thread::sleep(Duration::from_millis(200));
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_finished_unknown_id_treated_as_finished() {
        // all_finished 只判断 Running，不存在的 id 不算 Running → 返回 true。
        // 「缺失」语义由 wait_tasks 在轮询前单独检测，避免混入「仍在运行」导致无限等待。
        let mut reg = TaskRegistry::default();
        assert!(reg.all_finished(&["does-not-exist".to_string()]));
    }

    #[test]
    fn wait_tasks_unknown_id_without_timeout_does_not_block() {
        // 回归：不存在的 task id 即使 timeout_ms=0（无限等待语义）也必须立即返回，
        // 不能进入无限轮询。handle_wait 据此（results.len() < requested）报错。
        // 用一个看门狗线程兜底：若 5s 内未返回即判定卡死。
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let results = wait_tasks(vec!["no-such-task".to_string()], 0);
            let _ = tx.send(results);
        });
        let results = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("wait_tasks 对不存在的 id 在无 timeout 时卡住（回归）");
        assert!(
            results.is_empty(),
            "不存在的 task id 应返回空结果，实际：{results:?}"
        );
    }

    #[test]
    fn wait_tasks_unknown_id_with_timeout_returns_empty() {
        // 保留 timeout 路径的覆盖（即便 timeout 较大，缺失检测应先于超时返回）。
        let results = wait_tasks(vec!["no-such-task".to_string()], 60_000);
        assert!(results.is_empty(), "不存在的 task id 应返回空结果");
    }
}
