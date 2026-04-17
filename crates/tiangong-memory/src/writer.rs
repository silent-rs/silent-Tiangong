//! Episode 写入器
//!
//! 负责从 TurnResult 提取 Episode 并写入 Memory 存储。
//! Phase B：直接使用 turn 内容作为摘要（不调用 LLM 提取），后续版本升级为 LLM 摘要。

use crate::types::{Episode, EpisodeOutcome, TurnResult};

/// 从 TurnResult 提取 Episode
///
/// 仅在 `turn_result.had_tool_calls == true` 时才生成 Episode。
pub(crate) fn extract_episode(turn_result: &TurnResult) -> Option<Episode> {
    if !turn_result.had_tool_calls {
        return None;
    }

    let title = derive_title(&turn_result.summary);
    let outcome = EpisodeOutcome::Success; // Phase B 默认成功；Phase C 可由 LLM 判定
    let importance = estimate_importance(turn_result);
    let keywords = extract_keywords(&turn_result.summary);

    Some(Episode::new(
        turn_result.session_id.clone(),
        title,
        turn_result.summary.clone(),
        outcome,
        keywords,
        Vec::new(), // tool_calls 在 Phase B 中暂不填充，Phase C 接入完整数据
        importance,
    ))
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
    if turn_result.had_tool_calls { 0.5 } else { 0.1 }
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
