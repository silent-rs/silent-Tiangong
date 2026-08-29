//! @提及候选数据结构。
//!
//! 供 Core（`MentionCandidateProvider`）与 UI 输入补全共享。Core 在
//! `get_mentions()` 中遍历插件收集，src-tauri 的 `get_mention_candidates`
//! 命令经 Core 取回后返回前端。

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
