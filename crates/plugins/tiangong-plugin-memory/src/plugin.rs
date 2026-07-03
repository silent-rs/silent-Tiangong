//! 记忆召回插件：结构体定义与生命周期钩子实现。
//!
//! [`MemoryPlugin`] 通过以下方式获取运行时上下文：
//! - **构造注入**：`memory_handle` 在插件构造时由入口层注入（入口层负责 init）。
//! - [`Plugin::set_feedback_tx`]：注入状态反馈通道（用于转发流事件）。
//! - [`Plugin::on_config_updated`]：config 变化时热更新 memory actor。
//! - [`ToolOverrideHandler::handle`] 的 `&Session` 参数：按调用获取会话消息。
//!
//! ## 共享与隔离模型
//!
//! `memory_handle` 与 `feedback_tx` 是**跨 session 共享**的全局资源——记忆数据
//!（知识点、技能、跨会话经验）本就是用户级全局状态，多 session 共享同一
//! memory actor 单例是正确设计。
//!
//! `session_states` 按 session_id 隔离 per-session 控制流状态（recall_attempted /
//! turn_count），避免多 session 并发互相覆盖。
//!
//! ## 写记忆业务
//!
//! 反刍/候选由生命周期钩子接管：
//! - [`Plugin::on_turn_finished`]：评估本轮候选 + 异步触发增强版 Micro 反刍，
//!   每 10 turn 额外触发 Meta 反刍（归档低活跃节点）。
//! - [`Plugin::on_session_ended`]：触发 Meso 反刍（提炼 Entity/Decision）。

use std::collections::HashMap;
use std::sync::RwLock;

use crate::turn_extract::build_turn_memory_result;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::core::Plugin;
use tiangong_core::core_config::CoreConfig;
use tiangong_core::session::Session;
use tiangong_core::tool_override::PromptSectionProvider;
use tiangong_memory::MemoryHandle;

/// 每 N 个 turn 触发一次 Meta 反刍（归档低活跃节点）。
const META_RUMINATION_INTERVAL: u32 = 10;

/// per-session 控制流状态（按 session_id 隔离）。
#[derive(Default)]
struct SessionState {
    /// 本轮已回忆标志（去重用）。每轮 on_turn_started 重置为 false。
    recall_attempted: bool,
    /// turn 计数器（定期触发 Meta 反刍）。
    turn_count: u32,
}

/// 记忆召回插件。
///
/// `memory_handle` 在构造时由入口层注入（构造后不可变，跨 session 共享同一
/// memory actor 单例）；`feedback_tx` 由 core 在 register 时注入（跨 session 共享）；
/// `session_states` 按 session_id 隔离 per-session 控制流状态。
pub struct MemoryPlugin {
    /// 记忆句柄（构造时注入，内部 Arc 跨 session 共享）。None 表示记忆系统未启用。
    memory_handle: Option<MemoryHandle>,
    /// 状态反馈通道（转发 MemoryRecall* 流事件，由 set_feedback_tx 注入）。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
    /// per-session 控制流状态（recall_attempted + turn_count），按 session_id 隔离。
    session_states: RwLock<HashMap<String, SessionState>>,
}

impl MemoryPlugin {
    /// 构造插件实例。`memory_handle` 由入口层经
    /// [`tiangong_memory::registry::init_memory_handle_for_process`] 初始化后传入。
    pub fn new(memory_handle: Option<MemoryHandle>) -> Self {
        Self {
            memory_handle,
            feedback_tx: RwLock::new(None),
            session_states: RwLock::new(HashMap::new()),
        }
    }

    /// 读取记忆句柄的 clone（供 handler 检索用）。
    pub(crate) fn memory_handle(&self) -> Option<MemoryHandle> {
        self.memory_handle.clone()
    }

    /// 读取反馈通道的 clone（供 handler 发流事件用）。
    pub(crate) fn feedback_tx(&self) -> Option<PluginFeedbackTx> {
        self.feedback_tx.read().ok()?.as_ref().cloned()
    }

    /// 标记指定 session 本轮已完成回忆。
    ///
    /// 返回操作前的旧值（true 表示本轮已回忆过，应走去重分支）。
    pub(crate) fn mark_recall_attempted(&self, session_id: &str) -> bool {
        let Ok(mut guard) = self.session_states.write() else {
            return true;
        };
        let state = guard.entry(session_id.to_string()).or_default();
        let was_attempted = state.recall_attempted;
        state.recall_attempted = true;
        was_attempted
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

    fn on_config_updated(&self, config: &CoreConfig) {
        // config 变化时热更新 memory actor（reconfigure 模型/embedding/rerank 配置）。
        // 异步执行不阻塞 worker，失败仅告警（记忆功能降级而非中断）。
        if let Some(handle) = &self.memory_handle {
            let options = config.to_memory_options();
            let handle = handle.clone();
            tokio::spawn(async move {
                if let Err(e) = handle.reconfigure(options).await {
                    tracing::warn!(error = %e, "Memory actor reconfigure 失败");
                }
            });
        }
    }

    // register 留空：工具规格 / 工具覆盖 / Prompt 段落由 core 通过 supertrait 自动收集。

    fn on_turn_started(&self, session: &mut Session, _turn_start_idx: usize) {
        // 每轮重置该 session 的「已回忆」标志，允许新的一轮重新调用 recall_memory。
        if let Ok(mut guard) = self.session_states.write() {
            guard
                .entry(session.id.clone())
                .or_default()
                .recall_attempted = false;
        }
    }

    fn on_turn_finished(&self, session: &mut Session, turn_start_idx: usize) {
        let Some(handle) = self.memory_handle.clone() else {
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

        // 异步反刍：不阻塞 worker 收尾。反刍在 memory actor 内部排队执行，
        // 结果会在后续 recall 时自然可见（非强实时）。
        tokio::spawn(async move {
            handle.run_enhanced_micro_rumination(enhanced_result).await;
        });

        // 每 N 个 turn 触发一次 Meta 反刍（归档低活跃节点）。
        // run_meta_rumination 本身是 try_send fire-and-forget，不阻塞。
        if let Ok(mut guard) = self.session_states.write() {
            let state = guard.entry(session.id.clone()).or_default();
            state.turn_count += 1;
            if state.turn_count.is_multiple_of(META_RUMINATION_INTERVAL) {
                if let Some(h) = &self.memory_handle {
                    h.run_meta_rumination();
                    tracing::debug!(turn_count = state.turn_count, "Meta 反刍已触发（定期归档）");
                }
            }
        }
    }

    fn on_session_ended(&self, session: &mut Session) {
        if let Some(handle) = &self.memory_handle {
            // 会话结束 → 触发 Meso 反刍（提炼 Entity/Decision，更新 Workspace Injection）。
            // fire-and-forget：handle 仍可使用（Memory Actor 在 registry 中持续运行）。
            handle.run_meso_rumination(session.id.clone(), session.cwd.clone());
        }
        // 清理该 session 的控制流状态。
        if let Ok(mut guard) = self.session_states.write() {
            guard.remove(&session.id);
        }
    }
}

// recall_memory 无独立 Prompt 段落（使用指引已内嵌在工具 description 中），
// 采用默认空实现即可。
impl PromptSectionProvider for MemoryPlugin {}
