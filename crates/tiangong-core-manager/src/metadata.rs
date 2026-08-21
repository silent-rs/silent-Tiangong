//! 会话元数据：UI 展示与配置构建所需的轻量视图。
//!
//! 完整 `Session`（含 messages/context_summary/token 累计等）是磁盘真相源，
//! app-state 不再持有完整列表。本结构只承载 UI 列表展示与会话级配置构建
//! 必需的字段，作为磁盘的 **只读缓存视图**（可短暂数秒延迟）。

use std::path::Path;

use serde::{Deserialize, Serialize};
use tiangong_core::session::{Session, SessionCwdMode};
use tiangong_types::TrustMode;

/// 会话元数据：UI 展示 + 配置构建必需集。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub trust_mode: TrustMode,
    /// 会话级思考强度；为空时使用应用级默认值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<tiangong_llm::request::ReasoningEffort>,
    /// 会话级工作目录（工具执行时的根目录）。
    #[serde(default)]
    pub cwd: String,
    /// 工作目录模式。
    #[serde(default)]
    pub cwd_mode: SessionCwdMode,
    /// 消息条数（UI 列表展示）。P3 移除完整 Session 后需另从磁盘/缓存取。
    #[serde(default)]
    pub message_count: usize,
    /// 父会话 ID（Worker 子会话标注；UI 列表按此过滤掉子会话）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

impl From<&Session> for SessionMetadata {
    fn from(session: &Session) -> Self {
        Self {
            id: session.id.clone(),
            title: session.title.clone(),
            created_at: session.created_at.clone(),
            updated_at: session.updated_at.clone(),
            trust_mode: session.trust_mode,
            reasoning_effort: session.reasoning_effort.clone(),
            cwd: session.cwd.clone(),
            cwd_mode: session.cwd_mode.clone(),
            message_count: session.messages.len(),
            parent_session_id: session.parent_session_id.clone(),
        }
    }
}

impl SessionMetadata {
    /// 从磁盘 session 文件构造元数据（只读浅字段，不反序列化 messages 等重组件）。
    ///
    /// 复用 `Session::session_file_path` 的路径校验，但读取后只解析为
    /// `serde_json::Value` 取浅字段，避免反序列化完整的 `Session`（尤其 `messages`）。
    pub fn load_from_storage(storage_root: &Path, session_id: &str) -> Result<Self, String> {
        let path = crate::session_file_path(storage_root, session_id)?;
        let content = std::fs::read_to_string(&path)
            .map_err(|error| format!("读取会话文件失败（{}）：{error}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&content)
            .map_err(|error| format!("解析会话文件失败（{}）：{error}", path.display()))?;
        Ok(Self::from_value(&value))
    }

    /// 从已解析的 JSON Value 提取浅字段。
    fn from_value(value: &serde_json::Value) -> Self {
        let pick_str = |field: &str| -> String {
            value
                .get(field)
                .and_then(|item| item.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let message_count = value
            .get("messages")
            .and_then(|item| item.as_array())
            .map(|array| array.len())
            .unwrap_or(0);
        Self {
            id: pick_str("id"),
            title: pick_str("title"),
            created_at: pick_str("created_at"),
            updated_at: pick_str("updated_at"),
            trust_mode: value
                .get("trust_mode")
                .and_then(|item| serde_json::from_value(item.clone()).ok())
                .unwrap_or_default(),
            reasoning_effort: value
                .get("reasoning_effort")
                .and_then(|item| item.as_str())
                .map(tiangong_llm::request::ReasoningEffort::parse_flexible),
            cwd: pick_str("cwd"),
            cwd_mode: value
                .get("cwd_mode")
                .and_then(|item| serde_json::from_value(item.clone()).ok())
                .unwrap_or_default(),
            message_count,
            parent_session_id: value
                .get("parent_session_id")
                .and_then(|item| item.as_str())
                .map(str::to_string),
        }
    }

    /// 索引 ID（用于按 id 查找）。
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrips_via_serde() {
        let meta = SessionMetadata {
            id: "s1".into(),
            title: "t".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-02T00:00:00Z".into(),
            trust_mode: TrustMode::FullTrust,
            reasoning_effort: Some(tiangong_llm::request::ReasoningEffort::High),
            cwd: "/tmp".into(),
            cwd_mode: SessionCwdMode::Inherit,
            message_count: 3,
            parent_session_id: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: SessionMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
    }

    #[test]
    fn metadata_omits_none_reasoning_effort() {
        let meta = SessionMetadata {
            id: "s1".into(),
            title: "t".into(),
            created_at: "c".into(),
            updated_at: "u".into(),
            trust_mode: TrustMode::default(),
            reasoning_effort: None,
            cwd: String::new(),
            cwd_mode: SessionCwdMode::Inherit,
            message_count: 0,
            parent_session_id: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("reasoning_effort"));
    }
}
