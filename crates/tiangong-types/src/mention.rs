//! @提及请求与候选：Core 定义插件契约，CoreManager 负责上下文与收集。
use serde::{Deserialize, Serialize};

/// @提及候选项。
///
/// - `value`：插入值，如 `@skill:xxx` / `@mcp:yyy`，命中后写入输入框
/// - `label`：展示名
/// - `kind`：类型标签，如 `skill` / `mcp` / `agent`
/// - `hint`：副标题（描述、工具数等）
/// - `mark`：候选标记（chip 角标字符，如 `S` / `M`），由插件提供；
///   为空时前端按 `kind` 回退默认标记
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionCandidate {
    pub value: String,
    pub label: String,
    pub kind: String,
    pub hint: String,
    #[serde(default)]
    pub mark: String,
}

/// @提及候选分组。
///
/// App 层按 `kind` 对插件提供的候选分组，供前端按组渲染（组标题 + 组内候选）。
/// - `kind`：分组类型标签，如 `skill` / `mcp` / `agent`
/// - `label`：组标题（展示用）
/// - `candidates`：组内候选（已按数量上限截断）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionGroup {
    pub kind: String,
    pub label: String,
    pub candidates: Vec<MentionCandidate>,
}

/// 输入目标不隐式回退：无效会话不能借用其他工作区。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MentionTarget {
    #[default]
    Global,
    Session {
        session_id: String,
    },
    Draft {
        workspace: String,
    },
}

/// 宿主解析后的本次查询上下文；不改变插件的会话执行状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionContext {
    pub session_id: Option<String>,
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MentionRequest {
    pub target: MentionTarget,
    pub query: String,
    pub allowed_kinds: Vec<String>,
    pub max_per_group: usize,
}

impl Default for MentionRequest {
    fn default() -> Self {
        Self {
            target: MentionTarget::Global,
            query: String::new(),
            allowed_kinds: Vec::new(),
            max_per_group: 50,
        }
    }
}

/// 传给插件的只读查询，query 匹配必须发生在数量截断之前。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionQuery {
    pub context: MentionContext,
    pub query: String,
    pub allowed_kinds: Vec<String>,
    pub max_per_group: usize,
}
