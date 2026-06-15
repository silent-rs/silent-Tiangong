use std::future::Future;
use std::pin::Pin;

use crate::model::{ToolCall, ToolSpec};
use crate::tool::ToolResult;

/// 工具覆盖处理器。
///
/// 当 Agent 调用指定工具时，优先使用注册的处理器替代默认行为。
/// Plugin 通过此机制注入浏览器获取能力，替代硬编码的工具名拦截。
pub trait ToolOverrideHandler: Send + Sync + 'static {
    /// 处理工具调用。返回 None 表示不拦截，由默认逻辑处理。
    ///
    /// `session_id` 标识当前对话，用于按对话路由终端 PTY 等场景。
    fn handle(
        &self,
        call: &ToolCall,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>>;
}

/// 工具规格提供者。
///
/// Plugin 通过此机制向 Agent 注入新的工具定义（ToolSpec）。
/// 注册后，新工具会与 core 内置工具合并，统一暴露给 LLM。
pub trait ToolSpecProvider: Send + Sync + 'static {
    /// 返回该 plugin 暴露的所有工具规格。
    fn tool_specs(&self) -> Vec<ToolSpec>;
}

/// Prompt 规则提供者。
///
/// Plugin 通过此机制向 system prompt 注入规则段落（如终端交互引导、浏览器使用规范等）。
/// 段落会按 plugin 注册顺序追加到 system prompt 中。
pub trait PromptSectionProvider: Send + Sync + 'static {
    /// 返回该 plugin 暴露的所有 prompt 段落（每段会作为独立块拼接到 system prompt）。
    fn prompt_sections(&self) -> Vec<String>;
}
