//! 统一任务数据模型
//!
//! `UnifiedTask` 是系统中所有任务（前台 Turn、后台命令、多代理 Worker）的统一表示。
//! `UnifiedTaskStatus` 覆盖完整生命周期状态机。

use serde::{Deserialize, Serialize};

/// 统一任务状态
///
/// 覆盖前台任务和后台任务的完整生命周期：
/// ```text
/// Queued → Running → Completed / Failed / Cancelled
///                  ↔ WaitingApproval
///                  ↔ Backgrounded
///                  ↔ Blocked
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum UnifiedTaskStatus {
    /// 已排队，等待执行
    #[default]
    Queued,
    /// 正在执行
    Running,
    /// 被阻塞（等待外部依赖或其他任务完成）
    Blocked {
        /// 阻塞原因
        reason: String,
    },
    /// 等待用户审批（高风险操作）
    WaitingApproval {
        /// 审批请求 ID
        request_id: String,
        /// 待审批的工具名
        tool_name: String,
    },
    /// 已移至后台运行
    Backgrounded,
    /// 执行完成
    Completed {
        /// 退出码（后台命令任务适用），前台任务为 0
        exit_code: i32,
    },
    /// 执行失败
    Failed {
        /// 错误描述
        error: String,
    },
    /// 已取消
    Cancelled,
}

impl UnifiedTaskStatus {
    /// 是否为终止状态
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled
        )
    }

    /// 是否正在等待外部输入
    pub fn is_waiting(&self) -> bool {
        matches!(self, Self::WaitingApproval { .. } | Self::Blocked { .. })
    }

    /// 是否正在活跃执行（Running 或 Backgrounded）
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Backgrounded)
    }

    /// 获取简短显示名
    pub fn display_name(&self) -> &str {
        match self {
            Self::Queued => "排队中",
            Self::Running => "执行中",
            Self::Blocked { .. } => "已阻塞",
            Self::WaitingApproval { .. } => "等待审批",
            Self::Backgrounded => "后台运行",
            Self::Completed { .. } => "已完成",
            Self::Failed { .. } => "已失败",
            Self::Cancelled => "已取消",
        }
    }
}

impl std::fmt::Display for UnifiedTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// 任务类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// 前台对话 Turn
    Turn,
    /// 后台命令执行
    BackgroundCommand,
    /// 多代理 Worker 子任务
    Worker,
}

/// 统一任务
///
/// 所有任务（前台、后台、Worker）的统一表示。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedTask {
    /// 任务唯一 ID
    pub id: String,
    /// 任务类型
    pub kind: TaskKind,
    /// 输入摘要
    pub input_summary: String,
    /// 负责执行的代理/模块标识
    pub agent_id: String,
    /// 当前状态
    pub status: UnifiedTaskStatus,
    /// 当前进度描述
    pub progress: String,
    /// 结果位置（文件路径、会话 ID 等）
    pub result_location: Option<String>,
    /// 关联的会话 ID
    pub session_id: Option<String>,
    /// 关联的工作目录
    pub work_dir: Option<String>,
    /// 创建时间
    pub created_at: String,
    /// 最后更新时间
    pub updated_at: String,
    /// 输出内容（stdout，适用于后台命令）
    pub stdout: String,
    /// 错误输出（stderr，适用于后台命令）
    pub stderr: String,
    /// 执行的命令（适用于后台命令）
    pub command: Option<String>,
}

impl UnifiedTask {
    /// 创建新任务
    pub fn new(kind: TaskKind, input_summary: String, agent_id: String) -> Self {
        let now = chrono::Local::now().naive_local().to_string();
        Self {
            id: scru128::new().to_string(),
            kind,
            input_summary,
            agent_id,
            status: UnifiedTaskStatus::Queued,
            progress: String::new(),
            result_location: None,
            session_id: None,
            work_dir: None,
            created_at: now.clone(),
            updated_at: now,
            stdout: String::new(),
            stderr: String::new(),
            command: None,
        }
    }

    /// 设置关联会话
    pub fn with_session(mut self, session_id: String) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// 设置工作目录
    pub fn with_work_dir(mut self, dir: String) -> Self {
        self.work_dir = Some(dir);
        self
    }

    /// 设置命令（后台命令任务）
    pub fn with_command(mut self, cmd: String) -> Self {
        self.command = Some(cmd);
        self
    }

    /// 更新状态（自动更新 updated_at）
    pub fn set_status(&mut self, status: UnifiedTaskStatus) {
        self.status = status;
        self.updated_at = chrono::Local::now().naive_local().to_string();
    }

    /// 更新进度描述
    pub fn set_progress(&mut self, progress: String) {
        self.progress = progress;
        self.updated_at = chrono::Local::now().naive_local().to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_task_defaults() {
        let task = UnifiedTask::new(TaskKind::Turn, "测试输入".into(), "runtime".into());
        assert_eq!(task.kind, TaskKind::Turn);
        assert_eq!(task.status, UnifiedTaskStatus::Queued);
        assert!(!task.id.is_empty());
        assert!(!task.created_at.is_empty());
    }

    #[test]
    fn status_terminal_states() {
        assert!(UnifiedTaskStatus::Completed { exit_code: 0 }.is_terminal());
        assert!(
            UnifiedTaskStatus::Failed {
                error: "err".into()
            }
            .is_terminal()
        );
        assert!(UnifiedTaskStatus::Cancelled.is_terminal());
        assert!(!UnifiedTaskStatus::Running.is_terminal());
        assert!(!UnifiedTaskStatus::Queued.is_terminal());
    }

    #[test]
    fn status_waiting_states() {
        assert!(
            UnifiedTaskStatus::WaitingApproval {
                request_id: "r1".into(),
                tool_name: "run_command".into(),
            }
            .is_waiting()
        );
        assert!(
            UnifiedTaskStatus::Blocked {
                reason: "等待依赖".into()
            }
            .is_waiting()
        );
        assert!(!UnifiedTaskStatus::Running.is_waiting());
    }

    #[test]
    fn status_active_states() {
        assert!(UnifiedTaskStatus::Running.is_active());
        assert!(UnifiedTaskStatus::Backgrounded.is_active());
        assert!(!UnifiedTaskStatus::Queued.is_active());
        assert!(!UnifiedTaskStatus::Completed { exit_code: 0 }.is_active());
    }

    #[test]
    fn status_serialization_roundtrip() {
        let status = UnifiedTaskStatus::WaitingApproval {
            request_id: "abc".into(),
            tool_name: "run_command".into(),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("waiting_approval"));
        let deserialized: UnifiedTaskStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, status);
    }

    #[test]
    fn task_serialization_roundtrip() {
        let task = UnifiedTask::new(TaskKind::BackgroundCommand, "编译项目".into(), "bg".into())
            .with_session("s1".into())
            .with_work_dir("/tmp".into())
            .with_command("cargo build".into());

        let json = serde_json::to_string(&task).unwrap();
        let deserialized: UnifiedTask = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, task.id);
        assert_eq!(deserialized.kind, TaskKind::BackgroundCommand);
        assert_eq!(deserialized.command, Some("cargo build".into()));
    }

    #[test]
    fn set_status_updates_timestamp() {
        let mut task = UnifiedTask::new(TaskKind::Turn, "test".into(), "rt".into());
        let original = task.updated_at.clone();
        // 短暂延迟确保时间戳不同
        std::thread::sleep(std::time::Duration::from_millis(10));
        task.set_status(UnifiedTaskStatus::Running);
        assert_ne!(task.updated_at, original);
        assert_eq!(task.status, UnifiedTaskStatus::Running);
    }

    #[test]
    fn display_name_non_empty() {
        let statuses = [
            UnifiedTaskStatus::Queued,
            UnifiedTaskStatus::Running,
            UnifiedTaskStatus::Blocked { reason: "r".into() },
            UnifiedTaskStatus::WaitingApproval {
                request_id: "r".into(),
                tool_name: "t".into(),
            },
            UnifiedTaskStatus::Backgrounded,
            UnifiedTaskStatus::Completed { exit_code: 0 },
            UnifiedTaskStatus::Failed { error: "e".into() },
            UnifiedTaskStatus::Cancelled,
        ];
        for s in &statuses {
            assert!(!s.display_name().is_empty());
            assert!(!format!("{s}").is_empty());
        }
    }
}
