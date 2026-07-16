//! Agent 命令与执行效果类型

/// 用户命令
pub enum Command {
    /// 发送消息(spawn turn task)
    Message {
        prepared: Vec<tiangong_types::ContentBlock>,
        message_id: Option<String>,
    },
    /// 取消当前执行
    Cancel,
    /// 审批响应
    #[allow(dead_code)]
    Approval { request_id: String, approved: bool },
    /// 运行时切换信任模式(即时生效到活跃 turn task)
    SetTrustMode(crate::permission::TrustMode),
    /// 手动触发上下文压缩
    #[allow(dead_code)]
    CompressContext,
    /// 清理上下文（重置摘要，LLM 下次只看到 system prompt）
    #[allow(dead_code)]
    ResetContext,
    /// 工具类内容自动注入（浏览器页面、终端用户操作等，不触发 turn）。
    InjectTool {
        tool_name: String,
        payload: serde_json::Value,
    },
    /// 插件投递的流事件。
    EmitStreamEvent(Box<tiangong_types::StreamEvent>),
    /// 插件内部模型调用产生的 token 用量。
    ReportUsage {
        usage: tiangong_types::TokenUsage,
        source: String,
        emit_event: bool,
    },
    /// 关闭
    Shutdown,
}

/// 命令排空后的副作用
pub enum PendingCommandEffect {
    None,
    MessagesInjected { current_agent_input: Option<String> },
    Terminate,
    Shutdown,
}
