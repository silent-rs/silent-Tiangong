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

/// 单个对话轮次的最终执行状态，仅持久化到用户消息（turn 锚点）。
///
/// 向后兼容：旧 session 反序列化时缺失该字段默认为 None，前端不展示状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnStatus {
    /// 正常完成（含总结阶段产出最终回复）。
    Success,
    /// 执行过程中出错。
    Failed,
    /// 用户主动取消。
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// 消息所属的执行阶段。
///
/// 用于前端区分 ReAct 工具执行阶段的过程消息与总结阶段的最终回复，
/// 实现消息分层展示。向后兼容：旧 session 缺失该字段时默认为 `Normal`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessagePhase {
    /// 默认值：旧消息或未标记阶段的消息。
    #[default]
    Normal,
    /// ReAct 工具执行阶段的消息（工具调用、工具结果、过程文本）。
    React,
    /// 总结阶段的最终回复（可复制）。
    Summary,
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
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    /// 消息内容，支持文本、图片、视频、音频、文件等多种类型混合排列。
    /// 向后兼容：旧格式 content 为 String 时自动包装为 `vec![ContentBlock::Text(string)]`。
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub reasoning_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
    /// 多 Worker 模式下标识消息所属 Worker
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
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
    /// 消息所属的执行阶段，用于前端消息分层展示。
    #[serde(default)]
    pub phase: MessagePhase,
    pub created_at: String,
    /// 该用户消息所属轮次的执行时长（毫秒）。仅持久化到用户消息（turn 锚点），
    /// 前端据此展示「执行总时长」，历史会话重新打开同样可见。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// 该轮次的最终状态。仅持久化到用户消息，便于前端直观区分成功/失败/取消。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_status: Option<TurnStatus>,
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

/// 向后兼容的 Message 反序列化：
/// - content 同时支持旧 String 格式与新 content blocks 数组（见 deserialize_content）；
/// - 旧 session 的顶层 `media` 数组在反序列化时直接并入 content 末尾（不再保留为独立字段）。
impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(default)]
        struct MessageRaw {
            id: String,
            role: MessageRole,
            #[serde(deserialize_with = "deserialize_content")]
            content: Vec<ContentBlock>,
            reasoning_content: String,
            reasoning_signature: Option<String>,
            worker_id: Option<String>,
            /// 旧格式 media 字段：反序列化时捕获，随即并入 content，不保留为结构字段。
            #[serde(default)]
            media: Vec<MediaAsset>,
            tool_calls: Vec<MessageToolCall>,
            tool_call_id: Option<String>,
            tool_name: Option<String>,
            tool_result_is_error: bool,
            compact: bool,
            phase: MessagePhase,
            created_at: String,
            elapsed_ms: Option<u64>,
            turn_status: Option<TurnStatus>,
        }

        impl Default for MessageRaw {
            fn default() -> Self {
                Self {
                    id: String::new(),
                    role: MessageRole::User,
                    content: Vec::new(),
                    reasoning_content: String::new(),
                    reasoning_signature: None,
                    worker_id: None,
                    media: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tool_name: None,
                    tool_result_is_error: false,
                    compact: false,
                    phase: MessagePhase::Normal,
                    created_at: String::new(),
                    elapsed_ms: None,
                    turn_status: None,
                }
            }
        }

        let mut raw = MessageRaw::deserialize(deserializer)?;
        // 旧格式 media 数组合并到 content 末尾（迁移），不再作为独立字段存在。
        if !raw.media.is_empty() {
            for asset in raw.media.drain(..) {
                raw.content.push(asset.to_content_block());
            }
        }

        Ok(Message {
            id: raw.id,
            role: raw.role,
            content: raw.content,
            reasoning_content: raw.reasoning_content,
            reasoning_signature: raw.reasoning_signature,
            worker_id: raw.worker_id,
            tool_calls: raw.tool_calls,
            tool_call_id: raw.tool_call_id,
            tool_name: raw.tool_name,
            tool_result_is_error: raw.tool_result_is_error,
            compact: raw.compact,
            phase: raw.phase,
            created_at: raw.created_at,
            elapsed_ms: raw.elapsed_ms,
            turn_status: raw.turn_status,
        })
    }
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
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            phase: MessagePhase::Normal,
            created_at: now_text(),
            elapsed_ms: None,
            turn_status: None,
        }
    }

    /// 构造带推理内容的消息。`reasoning` 通常仅对 Assistant 有意义。
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
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            tool_result_is_error: false,
            compact: false,
            phase: MessagePhase::Normal,
            created_at: now_text(),
            elapsed_ms: None,
            turn_status: None,
        }
    }

    /// 设置消息的执行阶段标记（链式调用）。
    pub fn with_phase(mut self, phase: MessagePhase) -> Self {
        self.phase = phase;
        self
    }

    // ── 语义构造器：按角色表达专属字段，减少非法组合 ──

    /// 构造 Tool 结果消息，一次性写入 tool 专属字段。
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        let mut msg = Self::new(MessageRole::Tool, content);
        msg.tool_call_id = Some(tool_call_id.into());
        msg.tool_name = Some(tool_name.into());
        msg.tool_result_is_error = is_error;
        msg
    }

    /// 在 User 消息上写入该轮次的执行时长与最终状态（turn 锚点）。
    /// 仅 User 消息会写入；debug 构建下非 User 调用会触发断言，release 下为空操作。
    pub fn set_turn_result(&mut self, elapsed_ms: u64, status: TurnStatus) {
        debug_assert_eq!(
            self.role,
            MessageRole::User,
            "set_turn_result should only be called on user messages"
        );
        if self.role == MessageRole::User {
            self.elapsed_ms = Some(elapsed_ms);
            self.turn_status = Some(status);
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

    /// 判断是否包含文件类附件（PDF/Office 等非图片媒体）。
    ///
    /// 文件附件统一走本地脚本解析，不内联进 LLM 请求（issue #149）。
    pub fn has_file_media(&self) -> bool {
        self.content.iter().any(|b| {
            matches!(
                b,
                ContentBlock::Media {
                    kind: MediaKind::File,
                    ..
                }
            )
        })
    }

    /// 从 content blocks 提取媒体资产（content blocks 是媒体的唯一真相源）。
    ///
    /// `append_message_with_*_media` 把附件存进 `content` 的 `ContentBlock::Media`，
    /// 不再保留独立的 media 字段。任何需要 media 列表的地方都必须经此提取。
    pub fn extract_media_assets(&self) -> Vec<MediaAsset> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Media {
                    kind,
                    url,
                    mime_type,
                    title,
                } => Some(MediaAsset {
                    kind: *kind,
                    url: url.clone(),
                    mime_type: mime_type.clone(),
                    title: title.clone(),
                    capability: None,
                }),
                ContentBlock::Text { .. } => None,
            })
            .collect()
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// 当前本地时间文本
pub fn now_text() -> String {
    chrono::Local::now().naive_local().to_string()
}
