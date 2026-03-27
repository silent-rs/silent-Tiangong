use anyhow::Result;

use super::super::*;
use super::common::with_isolated_state;

#[test]
fn report_run_helpers_preserve_existing_snapshot_fields() -> Result<()> {
    with_isolated_state("tiangong-run-snapshot-helpers", |_paths, state| {
        state.store.runtime.run.last_session_id = Some("session-1".to_string());
        state.store.runtime.run.last_task_id = Some("task-1".to_string());
        state.store.runtime.run.last_duration_ms = Some(123);
        state.store.runtime.run.last_result = Some("previous-result".to_string());
        state.store.runtime.run.last_plan = Some("previous-plan".to_string());
        state.store.runtime.run.last_tool_result = Some("previous-tool".to_string());

        state.report_run_failed("失败摘要", "mock-error");
        assert!(matches!(state.store.runtime.run.status, RunStatus::Failed));
        assert_eq!(state.store.runtime.run.summary, "失败摘要");
        assert_eq!(
            state.store.runtime.run.last_error.as_deref(),
            Some("mock-error")
        );
        assert_eq!(
            state.store.runtime.run.last_session_id.as_deref(),
            Some("session-1")
        );
        assert_eq!(
            state.store.runtime.run.last_tool_result.as_deref(),
            Some("previous-tool")
        );

        state.report_run_idle("恢复空闲");
        assert!(matches!(state.store.runtime.run.status, RunStatus::Idle));
        assert_eq!(state.store.runtime.run.summary, "恢复空闲");
        assert!(state.store.runtime.run.last_error.is_none());
        assert_eq!(
            state.store.runtime.run.last_task_id.as_deref(),
            Some("task-1")
        );
        assert_eq!(
            state.store.runtime.run.last_plan.as_deref(),
            Some("previous-plan")
        );
        Ok(())
    })
}
