//! 外部与 Agent 交互的统一通道。
//!
//! 所有外部输入（用户消息、审批响应、终端操作、浏览器注入等）都经此 trait 投递。
//! `TiangongCore` 实现此 trait，内部转为 `Command` 发送到 worker 通道。
//!
//! 按与 Agent 交互的语义分为四层：
//! - [`AgentInputKind::Message`]：对话消息（触发 turn）
//! - [`AgentInputKind::Tool`]：工具类输入（伪造 tool result 注入对话，不触发 turn）
//! - [`AgentInputKind::Approval`]：审批响应（解锁阻塞等待的 turn）
//! - [`AgentInputKind::Command`]：控制指令（预留：cancel/reset 等控制类）

/// 外部与 Agent 交互的统一通道。
pub trait AgentInput: Send + Sync {
    /// 投递一个外部输入到 Agent，返回是否成功进入通道。
    fn deliver(&self, input: AgentInputKind) -> bool;
}

/// 外部输入的顶层分类，按交互语义分四层。
pub enum AgentInputKind {
    /// 对话消息层：用户消息等，会触发 Agent 执行一轮 turn。
    Message(MessageInput),
    /// 工具类输入层：伪造 tool result 注入对话（不触发 turn），
    /// 如用户终端操作、浏览器内容变化。
    Tool(ToolInput),
    /// 审批层：审批响应，解锁阻塞等待审批的 turn。
    Approval(ApprovalInput),
    /// 控制层：控制指令（预留扩展，当前无变体）。
    Command(CommandInput),
}

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

/// 工具类输入层（注入对话，不触发 turn）。
pub enum ToolInput {
    /// 用户终端操作：用户在终端提交命令（回车截断）时触发。
    TerminalUserInput { command: String },
    /// 浏览器内容注入：页面加载完成或内容变化时自动注入。
    BrowserContent {
        title: String,
        url: String,
        text: String,
        tabs: Vec<(String, String, String)>,
        active_tab_id: Option<String>,
        feedback: Option<String>,
    },
}

/// 审批层输入。
pub enum ApprovalInput {
    /// 审批响应（解锁当前阻塞等待审批的 turn）。
    Response { request_id: String, approved: bool },
}

/// 控制层输入（预留扩展，当前无变体）。
///
/// 未来 cancel/reset 等控制类操作可从 TiangongCore 的独立 pub 方法
/// 迁移到此枚举，实现完全统一的交互入口。
pub enum CommandInput {}
