use std::future::Future;
use std::pin::Pin;

use crate::model::ToolCall;
use crate::tool::ToolResult;

/// 工具覆盖处理器。
///
/// 当 Agent 调用指定工具时，优先使用注册的处理器替代默认行为。
/// Plugin 通过此机制注入浏览器获取能力，替代硬编码的工具名拦截。
pub trait ToolOverrideHandler: Send + Sync + 'static {
    /// 处理工具调用。返回 None 表示不拦截，由默认逻辑处理。
    fn handle(&self, call: &ToolCall) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>>;
}
