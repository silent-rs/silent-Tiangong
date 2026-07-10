//! 外部与 Agent 交互的统一通道。
//!
//! 所有外部输入（用户消息、审批响应、终端操作、浏览器注入等）都经此 trait 投递。
//! `TiangongCore` 实现此 trait，内部转为 `Command` 发送到 worker 通道。
//!
//! 按与 Agent 交互的语义分为四层：
//! - [`AgentInputKind::Message`]：对话消息（触发 turn）
//! - [`AgentInputKind::Tool`]：工具类输入（伪造 tool result 注入对话，不触发 turn）
//! - [`AgentInputKind::Approval`]：审批响应（解锁阻塞等待的 turn）
//! - [`AgentInputKind::Command`]：控制指令

/// 外部与 Agent 交互的统一通道。
pub trait AgentInput: Send + Sync {
    /// 投递一个外部输入到 Agent。
    ///
    /// 成功进入命令通道返回 `Ok(())`；worker 已停止、通道关闭时返回
    /// [`crate::core::CoreError::WorkerStopped`]，调用方据此清理僵尸 core。
    fn deliver(&self, input: AgentInputKind) -> Result<(), crate::core::CoreError>;
}

/// 外部输入的顶层分类，按交互语义分四层。
pub enum AgentInputKind {
    /// 对话消息层：用户消息等，会触发 Agent 执行一轮 turn。
    Message(MessageInput),
    /// 工具类输入层：伪造 tool result 注入对话（不触发 turn）。
    /// 用 `Box<dyn ToolInput>` trait object：插件可定义 struct 实现类型安全的注入，
    /// 也可通过 `AgentInputKind::tool(name, json)` 匿名注入。
    /// 渲染由 core 的 render_tool_output 统一处理。
    Tool(Box<dyn ToolInput>),
    /// 审批层：审批响应，解锁阻塞等待审批的 turn。
    Approval(ApprovalInput),
    /// 控制层：控制指令。
    Command(CommandInput),
}

impl AgentInputKind {
    /// 便捷构造：工具类注入（tool_name + JSON payload），无需定义 struct 实现 ToolInput。
    ///
    /// 适合匿名/一次性注入。有复用需求的工具类型建议在插件侧定义 struct
    /// 实现 `ToolInput` trait（类型更安全、render 逻辑内聚）。
    pub fn tool(tool_name: impl Into<String>, payload: serde_json::Value) -> Self {
        struct AnonymousToolInput {
            name: String,
            payload: serde_json::Value,
        }
        impl ToolInput for AnonymousToolInput {
            fn tool_name(&self) -> &str {
                &self.name
            }
            fn render(&self) -> serde_json::Value {
                self.payload.clone()
            }
        }
        AgentInputKind::Tool(Box::new(AnonymousToolInput {
            name: tool_name.into(),
            payload,
        }))
    }

    /// 便捷构造：用户消息（触发 turn）。
    pub fn message(content: impl Into<String>) -> Self {
        AgentInputKind::Message(MessageInput::UserMessage {
            content: content.into(),
            message_id: None,
            media: Vec::new(),
        })
    }

    /// 便捷构造：带 ID + 媒体的用户消息（前端预生成 ID 时使用）。
    pub fn message_with_id(
        content: impl Into<String>,
        message_id: impl Into<String>,
        media: Vec<tiangong_types::MediaAsset>,
    ) -> Self {
        AgentInputKind::Message(MessageInput::UserMessage {
            content: content.into(),
            message_id: Some(message_id.into()),
            media,
        })
    }

    /// 便捷构造：审批响应。
    pub fn approval(request_id: impl Into<String>, approved: bool) -> Self {
        AgentInputKind::Approval(ApprovalInput::Response {
            request_id: request_id.into(),
            approved,
        })
    }

    /// 便捷构造：取消当前执行（cancel_flag 由 deliver 内部设置）。
    pub fn cancel() -> Self {
        AgentInputKind::Command(CommandInput::Cancel)
    }

    /// 便捷构造：取消指定 Agent。
    pub fn cancel_agent(role: impl Into<String>) -> Self {
        AgentInputKind::Command(CommandInput::CancelAgent { role: role.into() })
    }

    /// 便捷构造：更新工作目录。
    pub fn update_cwd(cwd: impl Into<String>) -> Self {
        AgentInputKind::Command(CommandInput::UpdateCwd { cwd: cwd.into() })
    }

    /// 便捷构造：重新加载配置。
    pub fn reload_config() -> Self {
        AgentInputKind::Command(CommandInput::ReloadConfig)
    }

    /// 便捷构造：手动触发上下文压缩。
    pub fn compress_context() -> Self {
        AgentInputKind::Command(CommandInput::CompressContext)
    }

    /// 便捷构造：重置上下文。
    pub fn reset_context() -> Self {
        AgentInputKind::Command(CommandInput::ResetContext)
    }
}

// ===== Message 层 =====

/// 对话消息层输入。
pub enum MessageInput {
    /// 用户消息（触发 Agent 执行一轮 turn）。
    UserMessage {
        content: String,
        /// 前端预生成的消息 ID（用于流式复用），None 则由后端生成。
        message_id: Option<String>,
        media: Vec<tiangong_types::MediaAsset>,
    },
}

// ===== Approval 层 =====

/// 审批层输入。
pub enum ApprovalInput {
    /// 审批响应（解锁当前阻塞等待审批的 turn）。
    Response { request_id: String, approved: bool },
}

// ===== Command 层 =====

/// 控制层输入。
pub enum CommandInput {
    /// 取消当前执行。
    Cancel,
    /// 取消指定 Agent 的当前执行。
    CancelAgent { role: String },
    /// 更新当前会话工作目录。
    UpdateCwd { cwd: String },
    /// 重新加载共享配置。
    ReloadConfig,
    /// 手动触发上下文压缩。
    CompressContext,
    /// 清理上下文（重置摘要，LLM 下次只看到 system prompt）。
    ResetContext,
}

/// 工具类注入的统一协议。
///
/// core 定义此 trait，各插件（浏览器、终端等）在自己的 crate 里实现它，
/// 通过 `AgentInputKind::Tool(Box::new(XxxInput { ... }))` 投递。
/// core 的 worker 侧统一调用 `tool_name` + `render` 生成伪造的 tool result 消息，
/// 新增工具类型只需在插件侧实现此 trait，无需改动 core。
///
/// 对于一次性/匿名场景，可用 `AgentInputKind::tool(name, json)` 便捷构造。
pub trait ToolInput: Send + Sync {
    /// 工具名（伪造 tool_call 的 name 字段）。
    fn tool_name(&self) -> &str;

    /// 注入到对话的结构化内容（JSON）。
    ///
    /// 返回 JSON 而非文本，让 worker 侧根据 tool_name 决定呈现格式，
    /// 同时保留结构化数据供去重等逻辑使用。
    fn render(&self) -> serde_json::Value;
}
