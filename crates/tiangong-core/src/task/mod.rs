//! 统一任务模型
//!
//! 将 `RunStatus`（运行时状态）和 `background_task::TaskStatus`（后台任务状态）
//! 合并为统一的任务模型，覆盖完整生命周期状态机。

pub mod model;
pub mod notification;
pub mod persistence;
pub mod recovery;
pub mod registry;
pub mod state_machine;

pub use model::{TaskKind, UnifiedTask, UnifiedTaskStatus};
pub use notification::{TaskNotification, TaskNotificationBus, TaskNotificationType};
pub use registry::{UnifiedTaskRegistry, unified_task_registry};
pub use state_machine::TaskTransitionError;
