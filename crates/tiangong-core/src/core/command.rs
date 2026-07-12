//! Agent 命令与执行效果类型

/// 由 Core 单写者原子更新的会话元数据。
///
/// `reasoning_effort` 使用双层 `Option`：外层 `None` 表示不修改，
/// `Some(None)` 表示清除会话级覆盖。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionMetadataUpdate {
    pub title: Option<String>,
    pub trust_mode: Option<crate::permission::TrustMode>,
    pub reasoning_effort: Option<Option<String>>,
}

/// 用户命令
pub enum Command {
    /// 发送消息
    Message {
        prepared: Vec<tiangong_types::ContentBlock>,
        message_id: Option<String>,
        /// 继续处理已经持久化、且插件投递已完成的用户消息，不再次规划或追加。
        resume_existing: bool,
        /// 仅对该消息所属 turn 生效，轮次结束后恢复原模式。
        trust_mode_override: Option<crate::permission::TrustMode>,
        persistence_ack: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
    /// 更新当前会话工作目录
    UpdateCwd { cwd: String },
    /// 原子更新并持久化当前会话元数据。
    UpdateSessionMetadata {
        update: SessionMetadataUpdate,
        persistence_ack: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
    /// 重新加载共享配置
    ReloadConfig,
    /// 取消当前执行
    Cancel,
    /// 向指定插件发送结构化运行时控制指令。
    PluginControl {
        plugin_id: String,
        action: String,
        payload: serde_json::Value,
    },
    /// 审批响应
    #[allow(dead_code)]
    Approval { request_id: String, approved: bool },
    /// 手动触发上下文压缩
    #[allow(dead_code)]
    CompressContext,
    /// 清理上下文（重置摘要，LLM 下次只看到 system prompt）
    #[allow(dead_code)]
    ResetContext,
    /// 工具类内容自动注入（浏览器页面、终端用户操作等，不触发 turn）。
    ///
    /// 统一入口：tool_name + JSON payload 由 ToolInput trait 的 render 产出，
    /// worker 侧统一调用 inject_tool_to_session 处理。
    InjectTool {
        tool_name: String,
        payload: serde_json::Value,
    },
    /// 原子提交插件持久投递的完成状态与工具注入。
    ///
    /// Core 在父 Session 的同一次持久化中删除 `delivery_ids` 并把
    /// `tool_injections` 写入 deferred 队列。`persistence_ack` 仅在事务成功或
    /// 明确失败后响应，供插件避免先确认投递、后注入结果造成状态丢失。
    CommitPluginDeliveries {
        delivery_ids: Vec<String>,
        tool_injections: Vec<crate::core::plugin::PluginFeedback>,
        /// 本次提交用于明确取消这些投递，并原子标记来源用户消息为已取消。
        cancelled: bool,
        persistence_ack: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
    /// 插件投递的流事件（如 MemoryRecallStart/Progress/Done）。
    ///
    /// 插件通过 [`crate::core::plugin::feedback::PluginFeedbackTx::send_stream_event`]
    /// 投递，worker 收到后直接转发到 `stream_tx`，与 worker 自身发出的流事件
    /// 走同一出口。用于让插件复用 UI 实时事件通道，无需各自持有 stream_tx。
    EmitStreamEvent(Box<tiangong_types::StreamEvent>),
    /// 关闭
    Shutdown,
}

/// 命令排空后的副作用
pub enum PendingCommandEffect {
    None,
    MessagesInjected {
        current_agent_input: Option<String>,
        agent_routed: bool,
    },
    Terminate,
    Shutdown,
}
