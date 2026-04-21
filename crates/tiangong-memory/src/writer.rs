//! Episode 写入器
//!
//! 负责从 TurnResult 提取 Episode 并写入 Memory 存储。
//! Phase B：直接使用 turn 内容作为摘要（不调用 LLM 提取），后续版本升级为 LLM 摘要。

use crate::types::{Episode, EpisodeOutcome, TurnResult};

/// 从 TurnResult 提取 Episode
///
/// 仅在 `turn_result.had_tool_calls == true` 时才生成 Episode。
pub(crate) fn extract_episode(turn_result: &TurnResult) -> Option<Episode> {
    if !turn_result.had_tool_calls && turn_result.artifacts.is_empty() {
        return None;
    }

    let summary = build_episode_summary(turn_result);
    let title_source = if turn_result.user_input.trim().is_empty() {
        summary.as_str()
    } else {
        turn_result.user_input.as_str()
    };
    let title = derive_title(title_source);
    let outcome = EpisodeOutcome::Success; // Phase B 默认成功；Phase C 可由 LLM 判定
    let importance = estimate_importance(turn_result);
    let keywords = extract_keywords(&summary);

    Some(Episode::new(
        turn_result.session_id.clone(),
        title,
        summary,
        outcome,
        keywords,
        turn_result.tool_calls.clone(),
        importance,
    ))
}

fn build_episode_summary(turn_result: &TurnResult) -> String {
    let mut lines = Vec::new();
    if !turn_result.user_input.trim().is_empty() {
        lines.push(format!("用户请求: {}", turn_result.user_input.trim()));
    }
    if !turn_result.summary.trim().is_empty() {
        lines.push(format!("结果摘要: {}", turn_result.summary.trim()));
    }
    if !turn_result.tool_calls.is_empty() {
        lines.push(format!("工具调用: {}", turn_result.tool_calls.join(", ")));
    }
    if !turn_result.artifacts.is_empty() {
        lines.push("结构化产物:".to_string());
        for artifact in &turn_result.artifacts {
            let mut parts = vec![format!("{:?}", artifact.kind).to_lowercase()];
            if let Some(tool_name) = artifact.tool_name.as_deref() {
                parts.push(format!("tool={tool_name}"));
            }
            if let Some(title) = artifact.title.as_deref() {
                parts.push(format!("title={}", title.trim()));
            }
            if let Some(url) = artifact.url.as_deref() {
                parts.push(format!("url={}", url.trim()));
            }
            if let Some(path) = artifact.path.as_deref() {
                parts.push(format!("path={}", path.trim()));
            }
            if let Some(summary) = artifact.summary.as_deref() {
                parts.push(format!("summary={}", summary.trim()));
            }
            lines.push(format!("- {}", parts.join(" ")));
        }
    }
    lines.join("\n")
}

/// 从 summary 派生标题（取前 50 个字符）
fn derive_title(summary: &str) -> String {
    let trimmed = summary.trim();
    let title: String = trimmed.chars().take(50).collect();
    if title.len() < trimmed.len() {
        format!("{title}…")
    } else {
        title
    }
}

/// 估算重要度（0.0 ~ 1.0）
fn estimate_importance(turn_result: &TurnResult) -> f32 {
    // Phase B 简单规则：有工具调用的 turn 基础重要度 0.5
    if turn_result
        .artifacts
        .iter()
        .any(|artifact| artifact.url.is_some() || artifact.path.is_some())
    {
        0.8
    } else if turn_result.had_tool_calls {
        0.5
    } else {
        0.1
    }
}

/// 从文本中粗略提取关键词（去停用词后取前 10 个词）
fn extract_keywords(text: &str) -> Vec<String> {
    // 简单分词：按空白和常见标点拆分，过滤短词
    let stop_words = [
        "的", "了", "在", "是", "和", "有", "为", "与", "a", "the", "is", "in", "to", "of",
    ];
    let words: Vec<String> = text
        .split(|c: char| c.is_whitespace() || "，。！？；：、,.!?;:'\"()（）".contains(c))
        .filter(|w| w.len() >= 2)
        .filter(|w| !stop_words.contains(w))
        .take(10)
        .map(String::from)
        .collect();
    words
}
