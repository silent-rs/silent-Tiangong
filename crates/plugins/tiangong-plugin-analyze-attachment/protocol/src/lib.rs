//! Analyze-Attachment 插件私有业务协议。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PLUGIN_ID: &str = "analyze-attachment";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ATTACHMENT_PROTOCOL_VERSION: u32 = 1;

pub const TOOL_ANALYZE_ATTACHMENT: &str = "analyze_attachment";

pub trait AttachmentOperation {
    const NAME: &'static str;
    type Request: Serialize;
    type Response: DeserializeOwned;
}

pub const ANALYZE_OPERATION: &str = "analyze";

pub struct Analyze;

impl AttachmentOperation for Analyze {
    const NAME: &'static str = ANALYZE_OPERATION;
    type Request = AnalyzeRequest;
    type Response = AnalyzeResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

/// 附件分析请求。
///
/// 图片数据由 wasm 从会话消息提取后，以本地路径传给 sidecar；
/// sidecar 读取图片文件 → 构造多模态请求 → 调模型。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalyzeRequest {
    /// 解析指令（如"提取文字"、"描述画面"）。
    pub instruction: String,
    /// 用户原始消息文本（作为多模态请求上下文）。
    #[serde(default)]
    pub user_message_text: String,
    /// 图片本地路径列表（sidecar 读取后构造 ContentBlock::Image）。
    pub images: Vec<String>,
}

/// 分析响应。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalyzeResponse {
    /// 模型返回的分析文本。
    pub text: String,
    /// prompt token 用量（供 wasm 经 feedback 回传给 core）。
    #[serde(default)]
    pub prompt_tokens: u64,
    /// completion token 用量。
    #[serde(default)]
    pub completion_tokens: u64,
    /// 实际使用的模型名。
    pub model: String,
}
