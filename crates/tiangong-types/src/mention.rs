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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionCandidate {
    pub value: String,
    pub label: String,
    pub kind: String,
    pub hint: String,
}
