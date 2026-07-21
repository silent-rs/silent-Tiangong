//! 基础文件工具插件：聚合工具规格与覆盖处理器。
//!
//! [`FsPlugin`] 通过 [`Plugin::set_workspace`] 接收 core 注入的会话工作目录，
//! 通过 [`Plugin::set_trust_mode`] 接收 core 注入的会话信任解析句柄。工具规格与
//! 覆盖处理器直接在本类型上实现，core 通过 supertrait 自动收集。
//!
//! 写工具（`write_file` / `replace_in_file` / `apply_patch`）执行前由进程级共享的
//! [`FileLockTable`](crate::file_lock::FileLockTable) 对目标路径自动加锁、执行后
//! 自动解锁，对模型透明——锁的语义为「工具调用级文件操作互斥」，防止并发写
//! 同一文件互相覆盖。锁表跨所有 `FsPlugin` 实例共享（主 Agent 与子 Agent 互斥）。

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use tiangong_core::core::Plugin;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::permission::TrustMode;
use tiangong_types::StreamEvent;

/// 基础文件工具插件。
///
/// `workspace` 由 core 在 engine 创建及每次会话目录变更时注入（可变）；
/// `trust_mode` 由 core 在 register 前通过 `set_trust_mode` 注入（共享会话基线）。
/// `feedback` 由 core 每 turn 注入，用于发送 `FileLockChanged` 事件。
///
/// 文件锁状态不存于此实例——它由 [`crate::file_lock::FileLockTable`] 进程级
/// 全局表持有，跨所有 `FsPlugin` 实例共享。
#[derive(Default)]
pub struct FsPlugin {
    /// 当前会话工作目录（可变，由 core 注入）。
    workspace: RwLock<Option<PathBuf>>,
    /// 信任模式解析句柄（set_trust_mode 时注入，FullTrust 时放宽路径校验）。
    trust_mode: RwLock<Option<TrustMode>>,
    /// 当前 turn 的状态反馈通道（每 turn 由 core 注入，turn 结束后失效）。
    feedback: RwLock<Option<PluginFeedbackTx>>,
}

impl FsPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取当前工作目录的快照。
    pub(crate) fn workspace(&self) -> Option<PathBuf> {
        self.workspace.read().ok()?.clone()
    }

    /// 当前是否处于完全信任模式。
    pub(crate) fn is_full_trust(&self) -> bool {
        let Ok(handle) = self.trust_mode.read() else {
            return false;
        };
        let Some(tm) = handle.as_ref() else {
            return false;
        };
        *tm == TrustMode::FullTrust
    }

    /// 发送 `FileLockChanged` 事件到当前 turn 的反馈通道。
    ///
    /// 持有者相关字段（`holder_agent_id` / `holder_agent_label`）保持为空——
    /// 进程级锁不绑定 Agent 身份。若 feedback 未注入或通道已关闭则静默跳过。
    pub(crate) fn emit_file_lock_event(&self, path: &Path, action: &str) {
        let Some(feedback) = self.feedback.read().ok().and_then(|guard| guard.clone()) else {
            return;
        };
        feedback.send_stream_event(StreamEvent::FileLockChanged {
            path: path.display().to_string(),
            holder_agent_id: None,
            holder_agent_label: None,
            action: action.to_string(),
        });
    }
}

impl Plugin for FsPlugin {
    fn id(&self) -> &str {
        "fs"
    }

    fn set_workspace(&self, workspace: Option<&Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }

    fn set_trust_mode(&self, trust: TrustMode) {
        if let Ok(mut guard) = self.trust_mode.write() {
            *guard = Some(trust);
        }
    }

    fn set_feedback_tx(&self, feedback: PluginFeedbackTx) {
        if let Ok(mut current) = self.feedback.write() {
            *current = Some(feedback);
        }
    }

    // register 留空：信任模式已由 core 通过 set_trust_mode 统一注入，
    // 工具规格 / 工具覆盖 / Prompt 段落由 core 通过 supertrait 自动收集。
    // 过期锁的惰性清理发生在每次加锁时（FileLockTable::acquire 内 purge_expired）。
}

// PromptSectionProvider 使用默认空实现（fs 不注入 prompt 段落）
impl tiangong_core::tool_override::PromptSectionProvider for FsPlugin {}
