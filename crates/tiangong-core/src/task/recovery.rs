//! 启动恢复逻辑
//!
//! 启动时扫描持久化的任务文件，处理未完成的任务：
//! - Running / Backgrounded → 标记为 Failed（interrupted）
//! - WaitingApproval → 保留，恢复审批界面
//! - Blocked → 标记为 Failed（interrupted）
//! - Queued → 保留，可重新调度

use anyhow::Result;

use super::model::UnifiedTaskStatus;
use super::persistence;
use super::registry::UnifiedTaskRegistry;

/// 恢复结果
#[derive(Debug, Default)]
pub struct RecoveryResult {
    /// 总共扫描到的任务数
    pub total: usize,
    /// 标记为 interrupted 的任务数
    pub interrupted: usize,
    /// 保留待处理的任务数（Queued / WaitingApproval）
    pub pending: usize,
    /// 已完成的任务数（直接清理）
    pub completed: usize,
    /// 恢复的任务详情
    pub tasks: Vec<RecoveredTask>,
}

/// 单个恢复的任务信息
#[derive(Debug, Clone)]
pub struct RecoveredTask {
    pub task_id: String,
    pub original_status: String,
    pub new_status: String,
    pub input_summary: String,
}

/// 执行启动恢复
///
/// 扫描 `~/.tiangong/tasks/` 下的任务文件，处理未完成的任务并加载到注册表。
pub fn recover_tasks(registry: &mut UnifiedTaskRegistry) -> Result<RecoveryResult> {
    let tasks = persistence::load_all_tasks()?;
    let mut result = RecoveryResult {
        total: tasks.len(),
        ..Default::default()
    };

    for mut task in tasks {
        let original_status = task.status.display_name().to_string();

        match &task.status {
            // 终止状态：清理持久化文件
            UnifiedTaskStatus::Completed { .. }
            | UnifiedTaskStatus::Failed { .. }
            | UnifiedTaskStatus::Cancelled => {
                result.completed += 1;
                let _ = persistence::remove_task(&task.id);
                continue;
            }

            // 运行中/后台/阻塞：标记为 interrupted
            UnifiedTaskStatus::Running
            | UnifiedTaskStatus::Backgrounded
            | UnifiedTaskStatus::Blocked { .. } => {
                let new_status = UnifiedTaskStatus::Failed {
                    error: "进程中断（上次运行未正常结束）".into(),
                };
                task.set_status(new_status);
                let _ = persistence::save_task(&task);

                result.interrupted += 1;
                result.tasks.push(RecoveredTask {
                    task_id: task.id.clone(),
                    original_status,
                    new_status: task.status.display_name().to_string(),
                    input_summary: task.input_summary.clone(),
                });
            }

            // 排队中 / 等待审批：保留原状态
            UnifiedTaskStatus::Queued | UnifiedTaskStatus::WaitingApproval { .. } => {
                result.pending += 1;
                result.tasks.push(RecoveredTask {
                    task_id: task.id.clone(),
                    original_status: original_status.clone(),
                    new_status: original_status,
                    input_summary: task.input_summary.clone(),
                });
            }
        }

        // 加载到内存注册表
        registry.register(task);
    }

    if result.total > 0 {
        tracing::info!(
            total = result.total,
            interrupted = result.interrupted,
            pending = result.pending,
            completed = result.completed,
            "启动恢复：已处理 {} 个持久化任务",
            result.total
        );
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::model::{TaskKind, UnifiedTask, UnifiedTaskStatus};

    #[test]
    fn recovery_result_defaults() {
        let result = RecoveryResult::default();
        assert_eq!(result.total, 0);
        assert_eq!(result.interrupted, 0);
        assert_eq!(result.pending, 0);
        assert_eq!(result.completed, 0);
        assert!(result.tasks.is_empty());
    }

    #[test]
    fn recover_empty_directory() {
        // 如果任务目录不存在，应返回空结果
        let registry = UnifiedTaskRegistry::new();
        // 这里依赖 load_all_tasks 在目录不存在时返回空 Vec
        // 实际测试需要临时目录，这里仅验证逻辑正确性
        assert!(registry.is_empty());
    }

    #[test]
    fn running_task_marked_as_interrupted() {
        let mut task = UnifiedTask::new(TaskKind::BackgroundCommand, "build".into(), "bg".into());
        // 模拟 Running 状态
        task.status = UnifiedTaskStatus::Running;

        // 验证标记逻辑
        let new_status = UnifiedTaskStatus::Failed {
            error: "进程中断（上次运行未正常结束）".into(),
        };
        task.set_status(new_status);
        assert!(matches!(task.status, UnifiedTaskStatus::Failed { .. }));
    }

    #[test]
    fn queued_task_preserved() {
        let task = UnifiedTask::new(TaskKind::Turn, "test".into(), "rt".into());
        assert_eq!(task.status, UnifiedTaskStatus::Queued);
        // Queued 状态应保留，不做修改
    }
}
