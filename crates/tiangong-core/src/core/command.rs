//! Agent 命令与执行效果类型

/// 用户命令
pub(crate) enum Command {
    /// 发送消息
    Message {
        content: String,
        message_id: Option<String>,
        media: Vec<tiangong_types::MediaAsset>,
    },
    /// 更新当前会话工作目录
    UpdateCwd { cwd: String },
    /// 重新加载共享配置
    ReloadConfig,
    /// 取消当前执行
    Cancel,
    /// 审批响应
    #[allow(dead_code)]
    Approval { request_id: String, approved: bool },
    /// 关闭
    Shutdown,
}

/// 命令排空后的副作用
pub(crate) enum PendingCommandEffect {
    None,
    MessageInjected,
    Terminate,
}
