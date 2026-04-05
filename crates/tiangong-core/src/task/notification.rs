//! 任务完成通知机制
//!
//! 后台任务完成时生成 `RuntimeEvent`，支持通过 channel 发布，
//! 以便会话系统消费结果。

use std::sync::mpsc::{Receiver, Sender, channel};

use crate::event::{EventSource, RuntimeEvent, RuntimeEventType};

use super::model::{UnifiedTask, UnifiedTaskStatus};

/// 任务通知
#[derive(Debug, Clone)]
pub struct TaskNotification {
    /// 任务 ID
    pub task_id: String,
    /// 关联的会话 ID
    pub session_id: Option<String>,
    /// 通知类型
    pub notification_type: TaskNotificationType,
    /// 摘要信息
    pub summary: String,
}

/// 通知类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskNotificationType {
    /// 任务完成
    Completed,
    /// 任务失败
    Failed,
    /// 任务被取消
    Cancelled,
}

impl TaskNotification {
    /// 从统一任务生成通知（仅终止状态可生成）
    pub fn from_task(task: &UnifiedTask) -> Option<Self> {
        let notification_type = match &task.status {
            UnifiedTaskStatus::Completed { .. } => TaskNotificationType::Completed,
            UnifiedTaskStatus::Failed { .. } => TaskNotificationType::Failed,
            UnifiedTaskStatus::Cancelled => TaskNotificationType::Cancelled,
            _ => return None,
        };

        let summary = match &task.status {
            UnifiedTaskStatus::Completed { exit_code } => {
                format!("任务完成（exit_code: {exit_code}）")
            }
            UnifiedTaskStatus::Failed { error } => {
                format!("任务失败：{error}")
            }
            UnifiedTaskStatus::Cancelled => "任务已取消".to_string(),
            _ => unreachable!(),
        };

        Some(Self {
            task_id: task.id.clone(),
            session_id: task.session_id.clone(),
            notification_type,
            summary,
        })
    }

    /// 转换为 RuntimeEvent
    pub fn to_runtime_event(&self) -> Option<RuntimeEvent> {
        let session_id = self.session_id.clone()?;

        let event_type = match self.notification_type {
            TaskNotificationType::Completed => RuntimeEventType::TaskCompleted,
            TaskNotificationType::Failed => RuntimeEventType::TaskFailed,
            TaskNotificationType::Cancelled => RuntimeEventType::TaskFailed,
        };

        let payload = serde_json::json!({
            "task_id": self.task_id,
            "summary": self.summary,
        });

        Some(
            RuntimeEvent::new(event_type, session_id, EventSource::Runtime, payload)
                .with_task_id(self.task_id.clone()),
        )
    }
}

/// 任务通知总线
///
/// 提供 sender/receiver 对，用于跨模块传递任务完成通知。
pub struct TaskNotificationBus {
    sender: Sender<TaskNotification>,
    receiver: Receiver<TaskNotification>,
}

impl TaskNotificationBus {
    pub fn new() -> Self {
        let (sender, receiver) = channel();
        Self { sender, receiver }
    }

    /// 获取发送端（可 clone）
    pub fn sender(&self) -> Sender<TaskNotification> {
        self.sender.clone()
    }

    /// 消费接收端（只能调用一次）
    pub fn into_receiver(self) -> Receiver<TaskNotification> {
        self.receiver
    }

    /// 发送通知
    pub fn notify(&self, notification: TaskNotification) {
        let _ = self.sender.send(notification);
    }

    /// 尝试接收通知（非阻塞）
    pub fn try_recv(&self) -> Option<TaskNotification> {
        self.receiver.try_recv().ok()
    }
}

impl Default for TaskNotificationBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::model::{TaskKind, UnifiedTask};

    #[test]
    fn notification_from_completed_task() {
        let mut task = UnifiedTask::new(TaskKind::BackgroundCommand, "build".into(), "bg".into());
        task.session_id = Some("s1".into());
        task.set_status(UnifiedTaskStatus::Completed { exit_code: 0 });

        let notif = TaskNotification::from_task(&task).unwrap();
        assert_eq!(notif.notification_type, TaskNotificationType::Completed);
        assert_eq!(notif.session_id, Some("s1".into()));
    }

    #[test]
    fn notification_from_failed_task() {
        let mut task = UnifiedTask::new(TaskKind::BackgroundCommand, "build".into(), "bg".into());
        task.set_status(UnifiedTaskStatus::Failed {
            error: "编译失败".into(),
        });

        let notif = TaskNotification::from_task(&task).unwrap();
        assert_eq!(notif.notification_type, TaskNotificationType::Failed);
        assert!(notif.summary.contains("编译失败"));
    }

    #[test]
    fn notification_from_running_task_is_none() {
        let mut task = UnifiedTask::new(TaskKind::Turn, "test".into(), "rt".into());
        task.set_status(UnifiedTaskStatus::Running);
        assert!(TaskNotification::from_task(&task).is_none());
    }

    #[test]
    fn to_runtime_event_needs_session() {
        let mut task = UnifiedTask::new(TaskKind::BackgroundCommand, "build".into(), "bg".into());
        task.set_status(UnifiedTaskStatus::Completed { exit_code: 0 });

        let notif = TaskNotification::from_task(&task).unwrap();
        // 没有 session_id，不生成事件
        assert!(notif.to_runtime_event().is_none());
    }

    #[test]
    fn to_runtime_event_with_session() {
        let mut task = UnifiedTask::new(TaskKind::BackgroundCommand, "build".into(), "bg".into());
        task.session_id = Some("s1".into());
        task.set_status(UnifiedTaskStatus::Completed { exit_code: 0 });

        let notif = TaskNotification::from_task(&task).unwrap();
        let event = notif.to_runtime_event().unwrap();
        assert_eq!(event.event_type, RuntimeEventType::TaskCompleted);
        assert_eq!(event.session_id, "s1");
        assert_eq!(event.task_id, Some(task.id));
    }

    #[test]
    fn notification_bus_send_recv() {
        let bus = TaskNotificationBus::new();
        let notif = TaskNotification {
            task_id: "t1".into(),
            session_id: Some("s1".into()),
            notification_type: TaskNotificationType::Completed,
            summary: "done".into(),
        };
        bus.notify(notif.clone());
        let received = bus.try_recv().unwrap();
        assert_eq!(received.task_id, "t1");
    }

    #[test]
    fn notification_bus_empty() {
        let bus = TaskNotificationBus::new();
        assert!(bus.try_recv().is_none());
    }
}
