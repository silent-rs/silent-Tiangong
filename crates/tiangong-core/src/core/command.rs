//! Agent 命令与执行效果类型

use std::sync::Arc;

use crate::browser_trait::PageFetcher;
use crate::terminal_trait::TerminalProvider;
use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

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
    /// 取消指定 Agent 的当前执行
    CancelAgent { role: String },
    /// 审批响应
    #[allow(dead_code)]
    Approval { request_id: String, approved: bool },
    /// 手动触发上下文压缩
    #[allow(dead_code)]
    CompressContext,
    /// 清理上下文（重置摘要，LLM 下次只看到 system prompt）
    #[allow(dead_code)]
    ResetContext,
    /// 注入页面获取能力（GUI 模式下由 Tauri Plugin 提供）
    SetPageFetcher { fetcher: Arc<dyn PageFetcher> },
    /// 注入终端能力（GUI 模式下由 Tauri Plugin 提供）
    SetTerminalProvider { provider: Arc<dyn TerminalProvider> },
    /// 注册工具覆盖处理器
    RegisterToolOverride {
        name: String,
        handler: Arc<dyn ToolOverrideHandler>,
    },
    /// 注册工具规格提供者（plugin 注入新工具）
    RegisterToolSpecProvider { provider: Arc<dyn ToolSpecProvider> },
    /// 注册 Prompt 段落提供者（plugin 注入 system prompt 规则）
    RegisterPromptSectionProvider {
        provider: Arc<dyn PromptSectionProvider>,
    },
    /// 浏览器页面内容自动注入（页面加载完成时触发）
    InjectBrowserContent {
        title: String,
        url: String,
        text: String,
        tabs: Vec<(String, String, String)>,
        active_tab_id: Option<String>,
        feedback: Option<String>,
    },
    /// 关闭
    Shutdown,
}

/// 命令排空后的副作用
pub(crate) enum PendingCommandEffect {
    None,
    MessageInjected,
    Terminate,
}
