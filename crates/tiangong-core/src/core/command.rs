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
