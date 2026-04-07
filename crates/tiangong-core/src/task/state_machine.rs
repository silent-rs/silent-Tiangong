//! 任务状态转换验证
//!
//! 只允许合法的状态转换，防止无效状态跳转。

use super::model::UnifiedTaskStatus;

/// 状态转换错误
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTransitionError {
    pub from: String,
    pub to: String,
    pub reason: String,
}

impl std::fmt::Display for TaskTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "非法状态转换：{} → {}（{}）",
            self.from, self.to, self.reason
        )
    }
}

impl std::error::Error for TaskTransitionError {}

/// 验证状态转换是否合法
///
/// 合法转换图：
/// ```text
/// Queued → Running
/// Running → WaitingApproval / Backgrounded / Blocked / Completed / Failed / Cancelled
/// WaitingApproval → Running / Cancelled
/// Backgrounded → Running / Completed / Failed / Cancelled
/// Blocked → Running / Cancelled
/// ```
pub fn validate_transition(
    from: &UnifiedTaskStatus,
    to: &UnifiedTaskStatus,
) -> Result<(), TaskTransitionError> {
    let allowed = match from {
        UnifiedTaskStatus::Queued => matches!(
            to,
            UnifiedTaskStatus::Running | UnifiedTaskStatus::Cancelled
        ),

        UnifiedTaskStatus::Running => matches!(
            to,
            UnifiedTaskStatus::WaitingApproval { .. }
                | UnifiedTaskStatus::Backgrounded
                | UnifiedTaskStatus::Blocked { .. }
                | UnifiedTaskStatus::Completed { .. }
                | UnifiedTaskStatus::Failed { .. }
                | UnifiedTaskStatus::Cancelled
        ),

        UnifiedTaskStatus::WaitingApproval { .. } => {
            matches!(
                to,
                UnifiedTaskStatus::Running | UnifiedTaskStatus::Cancelled
            )
        }

        UnifiedTaskStatus::Backgrounded => matches!(
            to,
            UnifiedTaskStatus::Running
                | UnifiedTaskStatus::Completed { .. }
                | UnifiedTaskStatus::Failed { .. }
                | UnifiedTaskStatus::Cancelled
        ),

        UnifiedTaskStatus::Blocked { .. } => {
            matches!(
                to,
                UnifiedTaskStatus::Running | UnifiedTaskStatus::Cancelled
            )
        }

        // 终止状态不允许任何转换
        UnifiedTaskStatus::Completed { .. }
        | UnifiedTaskStatus::Failed { .. }
        | UnifiedTaskStatus::Cancelled => false,
    };

    if allowed {
        Ok(())
    } else {
        Err(TaskTransitionError {
            from: from.display_name().to_string(),
            to: to.display_name().to_string(),
            reason: "不允许的状态转换".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_to_running_allowed() {
        assert!(
            validate_transition(&UnifiedTaskStatus::Queued, &UnifiedTaskStatus::Running).is_ok()
        );
    }

    #[test]
    fn queued_to_completed_denied() {
        assert!(
            validate_transition(
                &UnifiedTaskStatus::Queued,
                &UnifiedTaskStatus::Completed { exit_code: 0 }
            )
            .is_err()
        );
    }

    #[test]
    fn running_to_all_next_states() {
        let running = UnifiedTaskStatus::Running;
        assert!(
            validate_transition(
                &running,
                &UnifiedTaskStatus::WaitingApproval {
                    request_id: "r".into(),
                    tool_name: "t".into()
                }
            )
            .is_ok()
        );
        assert!(validate_transition(&running, &UnifiedTaskStatus::Backgrounded).is_ok());
        assert!(
            validate_transition(&running, &UnifiedTaskStatus::Blocked { reason: "r".into() })
                .is_ok()
        );
        assert!(
            validate_transition(&running, &UnifiedTaskStatus::Completed { exit_code: 0 }).is_ok()
        );
        assert!(
            validate_transition(&running, &UnifiedTaskStatus::Failed { error: "e".into() }).is_ok()
        );
        assert!(validate_transition(&running, &UnifiedTaskStatus::Cancelled).is_ok());
    }

    #[test]
    fn waiting_approval_transitions() {
        let waiting = UnifiedTaskStatus::WaitingApproval {
            request_id: "r".into(),
            tool_name: "t".into(),
        };
        assert!(validate_transition(&waiting, &UnifiedTaskStatus::Running).is_ok());
        assert!(validate_transition(&waiting, &UnifiedTaskStatus::Cancelled).is_ok());
        assert!(
            validate_transition(&waiting, &UnifiedTaskStatus::Completed { exit_code: 0 }).is_err()
        );
    }

    #[test]
    fn backgrounded_transitions() {
        let bg = UnifiedTaskStatus::Backgrounded;
        assert!(validate_transition(&bg, &UnifiedTaskStatus::Running).is_ok());
        assert!(validate_transition(&bg, &UnifiedTaskStatus::Completed { exit_code: 0 }).is_ok());
        assert!(validate_transition(&bg, &UnifiedTaskStatus::Failed { error: "e".into() }).is_ok());
        assert!(validate_transition(&bg, &UnifiedTaskStatus::Cancelled).is_ok());
        assert!(validate_transition(&bg, &UnifiedTaskStatus::Queued).is_err());
    }

    #[test]
    fn terminal_states_deny_all() {
        let terminals = [
            UnifiedTaskStatus::Completed { exit_code: 0 },
            UnifiedTaskStatus::Failed { error: "e".into() },
            UnifiedTaskStatus::Cancelled,
        ];
        for t in &terminals {
            assert!(validate_transition(t, &UnifiedTaskStatus::Running).is_err());
            assert!(validate_transition(t, &UnifiedTaskStatus::Queued).is_err());
        }
    }

    #[test]
    fn error_display() {
        let err = TaskTransitionError {
            from: "已完成".into(),
            to: "执行中".into(),
            reason: "不允许的状态转换".into(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("已完成"));
        assert!(msg.contains("执行中"));
    }
}
