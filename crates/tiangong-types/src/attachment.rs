//! 宿主完成输入准备后交给 Agent 的通用消息合同。
//!
//! Core 只接收已经按最终顺序组织好的 [`ContentBlock`]，不参与资源处理方式、
//! 插件能力或提示文案决策。

use serde::{Deserialize, Serialize};

use crate::{ContentBlock, MediaKind};

/// 已由宿主保存的稳定资源引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAsset {
    pub asset_id: String,
    pub local_path: String,
    pub original_name: String,
    pub mime_type: String,
    pub size: u64,
    pub kind: MediaKind,
}

impl StoredAsset {
    pub fn has_inline_data_reference(&self) -> bool {
        [&self.asset_id, &self.local_path].into_iter().any(|value| {
            value
                .trim_start()
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
        })
    }

    pub fn clear_inline_data_reference(&mut self) {
        if self
            .local_path
            .trim_start()
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
        {
            self.local_path = "<inline-data-reference-unavailable>".to_string();
        }
        if self
            .asset_id
            .trim_start()
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
        {
            self.asset_id = "<inline-data-asset-unavailable>".to_string();
        }
    }
}

/// Core 用户消息入口：内容块已经由宿主准备完成并按最终顺序排列。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedUserMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
}

impl PreparedUserMessage {
    pub fn new(content: Vec<ContentBlock>) -> Self {
        Self { content }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![ContentBlock::text(text)])
    }

    /// 返回只含稳定内容的副本，移除所有仅供当前请求使用的图片数据。
    pub fn stable(&self) -> Self {
        let mut content = self.content.clone();
        for block in &mut content {
            block.clear_transient_data();
        }
        Self { content }
    }

    /// Core 接收边界校验：持久资源字段只能保存引用，不能伪装成内联数据通道。
    pub fn validate_ready(&self) -> Result<(), String> {
        for block in &self.content {
            block.validate_stable_reference()?;
        }
        Ok(())
    }

    /// 拼接面向用户的文本，不包含宿主提供给模型的指令块。
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(ContentBlock::as_text)
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn is_empty(&self) -> bool {
        self.content.iter().all(ContentBlock::is_empty)
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

    fn image_block(data: Option<&str>) -> ContentBlock {
        ContentBlock::Image {
            asset: StoredAsset {
                asset_id: "asset-1".to_string(),
                local_path: "/tmp/asset-1.png".to_string(),
                original_name: "asset-1.png".to_string(),
                mime_type: "image/png".to_string(),
                size: 4,
                kind: MediaKind::Image,
            },
            data: data.map(str::to_string),
        }
    }

    #[test]
    fn stable_message_clears_transient_image_data() {
        let prepared = PreparedUserMessage::new(vec![
            ContentBlock::text("查看图片"),
            image_block(Some("CURRENT_BASE64")),
        ]);

        let stable = prepared.stable();

        assert!(matches!(
            &prepared.content[1],
            ContentBlock::Image { data: Some(data), .. } if data == "CURRENT_BASE64"
        ));
        assert!(matches!(
            &stable.content[1],
            ContentBlock::Image { data: None, .. }
        ));
    }

    #[test]
    fn image_data_is_never_serialized() {
        let prepared = PreparedUserMessage::new(vec![image_block(Some("SECRET_BASE64"))]);
        let json = serde_json::to_string(&prepared).unwrap();

        assert!(!json.contains("SECRET_BASE64"));
        assert!(json.contains("/tmp/asset-1.png"));
    }

    #[test]
    fn ready_message_rejects_inline_data_in_stable_path() {
        let mut block = image_block(None);
        let ContentBlock::Image { asset, .. } = &mut block else {
            unreachable!();
        };
        asset.local_path = "data:image/png;base64,SECRET_BASE64".to_string();
        let prepared = PreparedUserMessage::new(vec![block]);

        assert!(prepared.validate_ready().is_err());
        let json = serde_json::to_string(&prepared.stable()).unwrap();
        assert!(!json.contains("SECRET_BASE64"));
        assert!(json.contains("inline-data-reference-unavailable"));
    }
}
