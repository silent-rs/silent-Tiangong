//! 消息类型

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;

/// 消息角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// 消息内容块
///
/// 统一表达消息中的文本、图片、视频、音频、文件等内容。
/// `Message.content` 为 `Vec<ContentBlock>`，支持多类型内容连续排列。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Media {
        kind: MediaKind,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

impl ContentBlock {
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text {
            text: content.into(),
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text { .. })
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Media { url, .. } => Some(url),
            Self::Text { .. } => None,
        }
    }

    pub fn kind(&self) -> Option<MediaKind> {
        match self {
            Self::Media { kind, .. } => Some(*kind),
            Self::Text { .. } => None,
        }
    }
}

/// 媒体类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    File,
}

/// 会话消息中的结构化媒体资源（旧格式，保留用于反序列化兼容）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub kind: MediaKind,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
}

impl MediaAsset {
    /// 转换为 ContentBlock
    pub fn to_content_block(&self) -> ContentBlock {
        ContentBlock::Media {
            kind: self.kind,
            url: self.url.clone(),
            mime_type: self.mime_type.clone(),
            title: self.title.clone(),
        }
    }
}

/// 对话消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    /// 消息内容，支持文本、图片、视频、音频、文件等多种类型混合排列。
    /// 向后兼容：旧格式 content 为 String 时自动包装为 `vec![ContentBlock::Text(string)]`。
    #[serde(deserialize_with = "deserialize_content")]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub reasoning_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
    /// 多 Worker 模式下标识消息所属 Worker
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    /// 旧格式 media 字段，反序列化时自动合并到 content。不再序列化。
    #[serde(
        default,
        deserialize_with = "deserialize_legacy_media",
        skip_serializing
    )]
    pub media: Vec<MediaAsset>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<MessageToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tool_result_is_error: bool,
    /// 表示从当前消息及以前的历史已被压缩摘要覆盖。
    #[serde(default, skip_serializing_if = "is_false")]
    pub compact: bool,
    pub created_at: String,
    /// 标记 media 是否已迁移到 content（避免重复迁移）
    #[serde(default, skip)]
    pub media_migrated: bool,
}

/// 向后兼容的 content 反序列化：
/// - 旧格式 `content: "some text"` → `vec![ContentBlock::Text("some text")]`
/// - 新格式 `content: [{...}, ...]` → 正常反序列化为 `Vec<ContentBlock>`
fn deserialize_content<'de, D>(deserializer: D) -> Result<Vec<ContentBlock>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ContentVisitor;

    impl<'de> de::Visitor<'de> for ContentVisitor {
        type Value = Vec<ContentBlock>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("string or array of content blocks")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(vec![ContentBlock::text(v.to_string())])
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(vec![ContentBlock::text(v)])
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            Deserialize::deserialize(de::value::SeqAccessDeserializer::new(seq))
        }
    }

    deserializer.deserialize_any(ContentVisitor)
}

/// 旧格式 media 反序列化：正常读取但不做额外处理。
/// 迁移逻辑在 `migrate_legacy_media` 中处理。
fn deserialize_legacy_media<'de, D>(deserializer: D) -> Result<Vec<MediaAsset>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<MediaAsset>::deserialize(deserializer)
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            id: scru128::new().to_string(),
            role,
            content: vec![ContentBlock::text(content.into())],
            reasoning_content: String::new(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            media_migrated: true,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            created_at: now_text(),
        }
    }

    pub fn with_reasoning(
        role: MessageRole,
        content: impl Into<String>,
        reasoning: impl Into<String>,
    ) -> Self {
        Self {
            id: scru128::new().to_string(),
            role,
            content: vec![ContentBlock::text(content.into())],
            reasoning_content: reasoning.into(),
            reasoning_signature: None,
            worker_id: None,
            media: Vec::new(),
            media_migrated: true,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            created_at: now_text(),
        }
    }

    /// 获取纯文本内容（拼接所有 Text 块）
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    /// 是否包含非文本内容块
    pub fn has_media(&self) -> bool {
        self.content.iter().any(|b| !b.is_text())
    }

    /// 将旧格式 media 字段迁移到 content 数组。
    /// 加载旧 session 后调用此方法完成自动升级。
    pub fn migrate_legacy_media(&mut self) {
        if self.media_migrated || self.media.is_empty() {
            self.media_migrated = true;
            return;
        }
        for asset in self.media.drain(..) {
            self.content.push(asset.to_content_block());
        }
        self.media_migrated = true;
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// 当前本地时间文本
pub fn now_text() -> String {
    chrono::Local::now().naive_local().to_string()
}
