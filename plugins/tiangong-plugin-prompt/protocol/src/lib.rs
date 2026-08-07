//! Prompt 插件私有协议（handle_view_message 路由用）。

use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "prompt";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// handle_view_message 的 method 常量。
pub const METHOD_GET_PROMPT: &str = "get_prompt";
pub const METHOD_SET_PROMPT: &str = "set_prompt";

/// get_prompt 响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetPromptResponse {
    /// custom-prompt.md 的完整内容（文件不存在时为空字符串）。
    pub content: String,
}

/// set_prompt 请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetPromptRequest {
    /// 新的 custom prompt 全文（空字符串表示清除）。
    pub content: String,
}
