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
    pub reasoning_effort: Option<String>,
    /// 会话级工作目录（工具执行时的根目录）。
    #[serde(default)]
    pub cwd: String,
    /// 工作目录模式。
    #[serde(default)]
    pub cwd_mode: SessionCwdMode,
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
        }
    }
}

impl SessionMetadata {
    /// 从磁盘 session 文件构造元数据（只读浅字段，不构造完整 Session）。
    ///
    /// 实现复用 `Session::load_from_storage` 的路径校验，保证与 worker 写盘路径
    /// 完全一致。storage_root 形如 `~/.tiangong`，session 文件位于
    /// `{storage_root}/sessions/{id}.json`。
    pub fn load_from_storage(storage_root: &Path, session_id: &str) -> Result<Self, String> {
        let session = Session::load_from_storage(storage_root, session_id)?;
        Ok(Self::from(&session))
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
            reasoning_effort: Some("high".into()),
            cwd: "/tmp".into(),
            cwd_mode: SessionCwdMode::Inherit,
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
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("reasoning_effort"));
    }
}
