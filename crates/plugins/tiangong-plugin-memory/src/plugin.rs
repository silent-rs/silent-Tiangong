//! 记忆召回插件：结构体定义与生命周期钩子实现。
//!
//! [`MemoryPlugin`] 通过三条注入通道获取运行时上下文：
//! - [`Plugin::set_memory_handle`]：注入记忆句柄（跨 turn 复用）。
//! - [`Plugin::set_feedback_tx`]：注入状态反馈通道（用于转发流事件）。
//! - [`ToolOverrideHandler::handle`] 的 `&Session` 参数：按调用获取会话消息。
//!
//! 「本轮已回忆」去重由 [`MemoryPlugin::recall_attempted`] 承载，
//! [`Plugin::on_turn_started`] 每轮重置为 false。
//!
//! 写记忆业务（反刍/候选）由生命周期钩子接管：
//! - [`Plugin::on_turn_finished`]：评估本轮候选 + 触发增强版 Micro 反刍，
//!   每 10 turn 额外触发 Meta 反刍（归档低活跃节点）。
//! - [`Plugin::on_session_ended`]：触发 Meso 反刍（提炼 Entity/Decision）。

use std::sync::RwLock;

use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::memory::turn_result::build_turn_memory_result;
use tiangong_core::session::Session;
use tiangong_core::tool_override::PromptSectionProvider;
use tiangong_memory::MemoryHandle;

/// 每 N 个 turn 触发一次 Meta 反刍（归档低活跃节点）。
const META_RUMINATION_INTERVAL: u32 = 10;

/// 记忆召回插件。
///
/// `memory_handle` 由 core 在 register 前注入（记忆系统启用时为 Some）；
/// `feedback_tx` 由 core 在 register 前注入（复用 worker 命令通道，转发流事件）；
/// `recall_attempted` 为「本轮已回忆」标志，每轮开始由 on_turn_started 重置；
/// `turn_count` 累计 turn 数，用于定期触发 Meta 反刍。
pub struct MemoryPlugin {
    /// 记忆句柄（内部 Arc，clone 后跨 turn 复用）。None 表示记忆系统未启用。
    memory_handle: RwLock<Option<MemoryHandle>>,
    /// 状态反馈通道（转发 MemoryRecall* 流事件）。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
    /// 本轮已回忆标志（去重用）。
    recall_attempted: RwLock<bool>,
    /// turn 计数器（用于定期触发 Meta 反刍）。
    turn_count: RwLock<u32>,
}

impl MemoryPlugin {
    /// 构造插件实例。记忆句柄与反馈通道在 register 时由 core 注入。
    pub fn new() -> Self {
        Self {
            memory_handle: RwLock::new(None),
            feedback_tx: RwLock::new(None),
            recall_attempted: RwLock::new(false),
            turn_count: RwLock::new(0),
        }
    }

    /// 读取记忆句柄的 clone（供 handler 检索用）。
    pub(crate) fn memory_handle(&self) -> Option<MemoryHandle> {
        self.memory_handle.read().ok()?.as_ref().cloned()
    }

    /// 读取反馈通道的 clone（供 handler 发流事件用）。
    pub(crate) fn feedback_tx(&self) -> Option<PluginFeedbackTx> {
        self.feedback_tx.read().ok()?.as_ref().cloned()
    }

    /// 标记本轮已完成回忆。返回操作前的旧值（true 表示本轮已回忆过，应走去重分支）。
    pub(crate) fn mark_recall_attempted(&self) -> bool {
        let mut guard = match self.recall_attempted.write() {
            Ok(g) => g,
            Err(_) => return true,
        };
        let was_attempted = *guard;
        *guard = true;
        was_attempted
    }
}

impl Default for MemoryPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for MemoryPlugin {
    fn id(&self) -> &str {
        "memory"
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
    }

    fn set_memory_handle(&self, handle: Option<MemoryHandle>) {
        if let Ok(mut guard) = self.memory_handle.write() {
            *guard = handle;
        }
    }

    // register 留空：工具规格 / 工具覆盖 / Prompt 段落由 core 通过 supertrait 自动收集。

    fn on_turn_started(&self, _session: &mut Session, _turn_start_idx: usize) {
        // 每轮重置「已回忆」标志，允许新的一轮重新调用 recall_memory。
        if let Ok(mut guard) = self.recall_attempted.write() {
            *guard = false;
        }
    }

    fn on_turn_finished(&self, session: &mut Session, turn_start_idx: usize) {
        let Some(handle) = self.memory_handle() else {
            return;
        };

        // 从本轮用户消息取 user_input（反刍摘要需要）。
        let user_input = session
            .messages
            .get(turn_start_idx)
            .filter(|m| m.role == tiangong_core::session::MessageRole::User)
            .map(|m| m.text_content())
            .unwrap_or_default();

        // 从 session 构建增强反刍结果（含候选评估，替代原 engine 的逐条 submit）。
        let enhanced_result = build_turn_memory_result(session, turn_start_idx, &user_input);
        tokio::task::block_in_place(|| {
            handle.run_enhanced_micro_rumination_blocking(enhanced_result);
        });

        // 每 N 个 turn 触发一次 Meta 反刍（归档低活跃节点）。
        if let Ok(mut count_guard) = self.turn_count.write() {
            *count_guard += 1;
            if (*count_guard).is_multiple_of(META_RUMINATION_INTERVAL) {
                handle.run_meta_rumination();
                tracing::debug!(turn_count = *count_guard, "Meta 反刍已触发（定期归档）");
            }
        }
    }

    fn on_session_ended(&self, session: &mut Session) {
        let Some(handle) = self.memory_handle() else {
            return;
        };
        // 会话结束 → 触发 Meso 反刍（提炼 Entity/Decision，更新 Workspace Injection）。
        // fire-and-forget：handle 仍可使用（Memory Actor 在 registry 中持续运行）。
        handle.run_meso_rumination(session.id.clone(), session.cwd.clone());
    }
}

// recall_memory 无独立 Prompt 段落（使用指引已内嵌在工具 description 中），
// 采用默认空实现即可。
impl PromptSectionProvider for MemoryPlugin {}
