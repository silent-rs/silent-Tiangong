//! core 插件适配：在 turn 结束时触发快照。
//!
//! `on_turn_finished` 由 core 在独立短命线程上调用（无 tokio runtime），
//! 这里只做非阻塞入队，实际拍摄由 [`crate::service::SnapshotService`] 的
//! 工作线程串行完成——满足"实现只做快速入队，重活交给插件自身后台任务"的约束。

use std::path::Path;
use std::sync::Arc;

use tiangong_core::core::plugin::Plugin;
use tiangong_core::session::Session;

use crate::formats::SnapshotReason;
use crate::service::SnapshotService;

pub struct SnapshotPlugin {
    service: Arc<SnapshotService>,
}

/// 构造快照插件（使用进程级单例服务）。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(SnapshotPlugin {
        service: SnapshotService::global(),
    })
}

// Plugin 的四个能力 supertrait 均为默认空实现，快照插件不提供工具/提示词/提及候选。
impl tiangong_core::tool_override::ToolOverrideHandler for SnapshotPlugin {}
impl tiangong_core::tool_override::ToolSpecProvider for SnapshotPlugin {}
impl tiangong_core::tool_override::PromptSectionProvider for SnapshotPlugin {}
impl tiangong_core::tool_override::MentionCandidateProvider for SnapshotPlugin {}

impl Plugin for SnapshotPlugin {
    fn id(&self) -> &str {
        "snapshot"
    }

    fn on_turn_finished(&self, session: &Session, turn_start_idx: usize) {
        let workspace = Path::new(&session.cwd);
        if !workspace.is_dir() {
            return;
        }
        self.service.request_snapshot(
            session.id.clone(),
            workspace.to_path_buf(),
            turn_start_idx,
            SnapshotReason::Turn,
        );
    }
}
