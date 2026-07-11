//! 用户附件经过宿主入口保存、规划后的稳定消息合同。
//!
//! `RawAttachment` 属于宿主 App 层，不在本 crate 中定义。Core 只接收
//! [`PreparedUserMessage`]，其中可持久化附件与仅供当前轮次使用的运行内容严格分离。

use serde::{Deserialize, Serialize};

use crate::MediaKind;

/// 附件进入模型请求或工具链路的确定处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentHandlingMode {
    /// 图片内容直接进入对话模型请求。
    InlineImage,
    /// 主模型只接收本地引用，并按需调用附件分析插件。
    AnalyzeWithPlugin,
    /// 主模型只接收稳定文件引用，由文件工具、文档解析器或 Skill 使用。
    FileReference,
}

/// 已由宿主入口保存并完成处理方案规划的稳定附件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedAttachment {
    pub asset_id: String,
    pub local_path: String,
    pub original_name: String,
    pub mime_type: String,
    pub size: u64,
    pub kind: MediaKind,
    pub handling_mode: AttachmentHandlingMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(default)]
    pub capability_available: bool,
}

/// 仅供当前轮次模型请求使用、不得写入会话文件的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeContent {
    InlineImage {
        asset_id: String,
        mime_type: String,
        /// 图片 base64；可为纯 base64，也可为完整 data URL。
        data: String,
    },
}

impl RuntimeContent {
    pub fn asset_id(&self) -> &str {
        match self {
            Self::InlineImage { asset_id, .. } => asset_id,
        }
    }
}

/// Core 用户消息入口：稳定附件与本轮运行内容分离。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedUserMessage {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persistent_attachments: Vec<PreparedAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_content: Vec<RuntimeContent>,
}

impl PreparedUserMessage {
    pub fn new(
        text: impl Into<String>,
        persistent_attachments: Vec<PreparedAttachment>,
        runtime_content: Vec<RuntimeContent>,
    ) -> Self {
        Self {
            text: text.into(),
            persistent_attachments,
            runtime_content,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::new(text, Vec::new(), Vec::new())
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.persistent_attachments.is_empty()
    }
}

impl From<String> for PreparedUserMessage {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<&str> for PreparedUserMessage {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handling_mode_serde_roundtrip_uses_snake_case() {
        let modes = [
            (AttachmentHandlingMode::InlineImage, "\"inline_image\""),
            (
                AttachmentHandlingMode::AnalyzeWithPlugin,
                "\"analyze_with_plugin\"",
            ),
            (AttachmentHandlingMode::FileReference, "\"file_reference\""),
        ];

        for (mode, expected) in modes {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, expected);
            assert_eq!(
                serde_json::from_str::<AttachmentHandlingMode>(&json).unwrap(),
                mode
            );
        }
    }
}
