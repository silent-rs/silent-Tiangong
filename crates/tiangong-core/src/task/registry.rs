//! 统一任务注册表
//!
//! 管理所有任务的创建、查询、状态更新和清理。
//! 替代原有 `tool::background_task::TaskRegistry` 的独立实现。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use super::model::{TaskKind, UnifiedTask, UnifiedTaskStatus};
use super::state_machine::validate_transition;

/// 全局统一任务注册表
static UNIFIED_REGISTRY: OnceLock<Arc<Mutex<UnifiedTaskRegistry>>> = OnceLock::new();

/// 获取全局统一任务注册表
pub fn unified_task_registry() -> Arc<Mutex<UnifiedTaskRegistry>> {
    UNIFIED_REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(UnifiedTaskRegistry::default())))
        .clone()
}

/// 统一任务注册表
#[derive(Debug, Default)]
pub struct UnifiedTaskRegistry {
    tasks: HashMap<String, UnifiedTask>,
}

impl UnifiedTaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册新任务
    pub fn register(&mut self, task: UnifiedTask) -> String {
        let id = task.id.clone();
        self.tasks.insert(id.clone(), task);
        id
    }

    /// 查询任务
    pub fn get(&self, task_id: &str) -> Option<&UnifiedTask> {
        self.tasks.get(task_id)
    }

    /// 获取可变引用
    pub fn get_mut(&mut self, task_id: &str) -> Option<&mut UnifiedTask> {
        self.tasks.get_mut(task_id)
    }

    /// 安全地更新任务状态（验证状态转换合法性）
    pub fn transition(
        &mut self,
        task_id: &str,
        new_status: UnifiedTaskStatus,
    ) -> Result<(), String> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("任务不存在：{task_id}"))?;

        validate_transition(&task.status, &new_status).map_err(|e| e.to_string())?;

        task.set_status(new_status);
        Ok(())
    }

    /// 列出所有任务
    pub fn list(&self) -> Vec<&UnifiedTask> {
        self.tasks.values().collect()
    }

    /// 按类型列出任务
    pub fn list_by_kind(&self, kind: &TaskKind) -> Vec<&UnifiedTask> {
        self.tasks.values().filter(|t| &t.kind == kind).collect()
    }

    /// 列出活跃任务（Running 或 Backgrounded）
    pub fn list_active(&self) -> Vec<&UnifiedTask> {
        self.tasks
            .values()
            .filter(|t| t.status.is_active())
            .collect()
    }

    /// 列出未完成任务（非终止状态）
    pub fn list_pending(&self) -> Vec<&UnifiedTask> {
        self.tasks
            .values()
            .filter(|t| !t.status.is_terminal())
            .collect()
    }

    /// 按会话 ID 列出任务
    pub fn list_by_session(&self, session_id: &str) -> Vec<&UnifiedTask> {
        self.tasks
            .values()
            .filter(|t| t.session_id.as_deref() == Some(session_id))
            .collect()
    }

    /// 清理已完成的任务
    pub fn cleanup_completed(&mut self) {
        self.tasks.retain(|_, t| !t.status.is_terminal());
    }

    /// 移除指定任务
    pub fn remove(&mut self, task_id: &str) -> Option<UnifiedTask> {
        self.tasks.remove(task_id)
    }

    /// 任务数量
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(kind: TaskKind) -> UnifiedTask {
        UnifiedTask::new(kind, "test".into(), "agent".into())
    }

    #[test]
    fn register_and_get() {
        let mut reg = UnifiedTaskRegistry::new();
        let task = make_task(TaskKind::Turn);
        let id = task.id.clone();
        reg.register(task);

        assert!(reg.get(&id).is_some());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn transition_valid() {
        let mut reg = UnifiedTaskRegistry::new();
        let task = make_task(TaskKind::Turn);
        let id = task.id.clone();
        reg.register(task);

        assert!(reg.transition(&id, UnifiedTaskStatus::Running).is_ok());
        assert_eq!(reg.get(&id).unwrap().status, UnifiedTaskStatus::Running);

        assert!(
            reg.transition(&id, UnifiedTaskStatus::Completed { exit_code: 0 })
                .is_ok()
        );
    }

    #[test]
    fn transition_invalid() {
        let mut reg = UnifiedTaskRegistry::new();
        let task = make_task(TaskKind::Turn);
        let id = task.id.clone();
        reg.register(task);

        // Queued → Completed 非法
        assert!(
            reg.transition(&id, UnifiedTaskStatus::Completed { exit_code: 0 })
                .is_err()
        );
    }

    #[test]
    fn transition_nonexistent() {
        let mut reg = UnifiedTaskRegistry::new();
        assert!(
            reg.transition("nonexistent", UnifiedTaskStatus::Running)
                .is_err()
        );
    }

    #[test]
    fn list_by_kind() {
        let mut reg = UnifiedTaskRegistry::new();
        reg.register(make_task(TaskKind::Turn));
        reg.register(make_task(TaskKind::BackgroundCommand));
        reg.register(make_task(TaskKind::Turn));

        assert_eq!(reg.list_by_kind(&TaskKind::Turn).len(), 2);
        assert_eq!(reg.list_by_kind(&TaskKind::BackgroundCommand).len(), 1);
    }

    #[test]
    fn list_active() {
        let mut reg = UnifiedTaskRegistry::new();
        let t1 = make_task(TaskKind::Turn);
        let t2 = make_task(TaskKind::Turn);
        let id1 = t1.id.clone();
        reg.register(t1);
        reg.register(t2);

        // t1 → Running
        reg.transition(&id1, UnifiedTaskStatus::Running).unwrap();

        assert_eq!(reg.list_active().len(), 1);
    }

    #[test]
    fn list_by_session() {
        let mut reg = UnifiedTaskRegistry::new();
        let t1 = make_task(TaskKind::Turn).with_session("s1".into());
        let t2 = make_task(TaskKind::Turn).with_session("s2".into());
        reg.register(t1);
        reg.register(t2);

        assert_eq!(reg.list_by_session("s1").len(), 1);
        assert_eq!(reg.list_by_session("s3").len(), 0);
    }

    #[test]
    fn cleanup_completed() {
        let mut reg = UnifiedTaskRegistry::new();
        let t1 = make_task(TaskKind::Turn);
        let t2 = make_task(TaskKind::Turn);
        let id1 = t1.id.clone();
        reg.register(t1);
        reg.register(t2);

        reg.transition(&id1, UnifiedTaskStatus::Running).unwrap();
        reg.transition(&id1, UnifiedTaskStatus::Completed { exit_code: 0 })
            .unwrap();

        reg.cleanup_completed();
        assert_eq!(reg.len(), 1); // 只剩 t2（Queued）
    }

    #[test]
    fn remove_task() {
        let mut reg = UnifiedTaskRegistry::new();
        let task = make_task(TaskKind::Turn);
        let id = task.id.clone();
        reg.register(task);

        let removed = reg.remove(&id);
        assert!(removed.is_some());
        assert!(reg.is_empty());
    }
}
