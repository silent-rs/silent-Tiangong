//! Agent 命令类型

/// 用户命令
pub enum Command {
    /// 取消当前执行
    Cancel,
    /// 审批响应
    #[allow(dead_code)]
    Approval { request_id: String, approved: bool },
    /// 运行时切换信任模式(即时生效到活跃 turn task)
    SetTrustMode(crate::permission::TrustMode),
    /// 运行时切换思考强度（下一次尚未发出的模型请求生效）。
    SetReasoningEffort(String),
    /// 更新会话标题。
    /// `only_if_default=true` 时仅当当前标题仍是默认值（"新对话"/"会话 X"）才覆盖，
    /// 用于 lite 自动生成（用户手动改过则不覆盖）；false 时无条件覆盖（用户手动编辑）。
    SetTitle {
        title: String,
        only_if_default: bool,
    },
    /// 工具类内容自动注入（浏览器页面、终端用户操作等，不触发 turn）。
    InjectTool {
        tool_name: String,
        payload: serde_json::Value,
    },
    /// 运行中注入用户消息：中断主循环直接拥有的活动（模型/工具等待/压缩/审批），
    /// 在同一物理 turn 内保存新消息并从新意图重启（ALR-101）。
    ///
    /// 执行线程校验并事务性保存消息，成功后才向界面发 `UserMessage` 确认——
    /// 调用方仅凭投递成功不能认定消息已进入会话。插件独立持有的后台任务不受
    /// 影响（ALR-103），只有显式取消才走 `on_cancel`。
    InjectUserMessage {
        message_id: String,
        content: Vec<tiangong_types::ContentBlock>,
    },
    /// 插件投递的流事件。
    EmitStreamEvent(Box<tiangong_types::StreamEvent>),
    /// 插件内部模型调用产生的 token 用量。
    ReportUsage {
        usage: tiangong_types::TokenUsage,
        source: String,
        emit_event: bool,
    },
    /// 手动压缩上下文；空闲时执行，运行中保留到当前 turn 结束后。
    CompressContext,
    /// 重置上下文；空闲时执行，运行中保留到当前 turn 结束后。
    ResetContext,
    /// 关闭
    Shutdown,
}
