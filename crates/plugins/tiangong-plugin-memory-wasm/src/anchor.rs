//! 检索锚点规则规划（自 `tiangong-memory` 下沉的 fallback 路径）。
//!
//! 对应宿主侧 `recall_anchor.rs` 的 `fallback_plan` / `fallback_keywords` /
//! `fallback_strategy` 等纯逻辑（无 LLM）。

use crate::bindings::exports::tiangong::plugin::plugin::{RecallAnchors, SearchStrategy};
use crate::text_utils::{
    contains_history_reference, dedupe_strings, extract_media_markers, extract_paths,
    extract_text_terms, extract_tool_names, extract_urls, is_precise_anchor,
};

/// 规划请求的等价输入（避免直接依赖宿主侧 MemoryRecallRequest）。
pub(crate) struct RecallInput<'a> {
    pub query: &'a str,
    pub reason: Option<&'a str>,
    pub expected: &'a [String],
    pub context: &'a [String],
}

/// 规划产出。
pub(crate) struct Planned {
    pub anchors: RecallAnchors,
    pub limit: u32,
    pub used_llm: bool,
}

/// 规则规划检索锚点（无 LLM）。
pub(crate) fn fallback_plan(input: &RecallInput<'_>, raw_limit: u32) -> Planned {
    let keywords = fallback_keywords(input);
    let strategy = fallback_strategy(input, &keywords);
    let query = if matches!(strategy, Some(SearchStrategy::Skip)) {
        String::new()
    } else {
        build_fallback_query(input, &keywords)
    };

    Planned {
        anchors: RecallAnchors {
            query,
            keywords,
            strategy,
        },
        limit: raw_limit.clamp(1, 10),
        used_llm: false,
    }
}

fn fallback_keywords(input: &RecallInput<'_>) -> Vec<String> {
    let mut keywords = input.expected.to_vec();
    let text = format!(
        "{}\n{}\n{}",
        input.query,
        input.reason.unwrap_or(""),
        input.context.join("\n")
    );

    keywords.extend(extract_urls(&text));
    keywords.extend(extract_paths(&text));
    keywords.extend(extract_tool_names(&text));
    keywords.extend(extract_media_markers(&text));
    keywords.extend(extract_text_terms(input.query, 8));
    dedupe_strings(keywords)
}

fn fallback_strategy(input: &RecallInput<'_>, keywords: &[String]) -> Option<SearchStrategy> {
    if input.query.trim().is_empty() || is_plain_chitchat(input) {
        return Some(SearchStrategy::Skip);
    }
    if contains_history_reference(input.query) {
        return Some(SearchStrategy::Semantic);
    }
    if keywords.iter().any(|item| is_precise_anchor(item)) {
        return Some(SearchStrategy::Keyword);
    }
    Some(SearchStrategy::Hybrid(0.6))
}

fn build_fallback_query(input: &RecallInput<'_>, keywords: &[String]) -> String {
    if !keywords.is_empty()
        && (contains_history_reference(input.query) || input.query.chars().count() > 80)
    {
        return keywords
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
    }
    input.query.to_string()
}

/// 是否为普通闲聊（无需检索）。
fn is_plain_chitchat(input: &RecallInput<'_>) -> bool {
    if !input.expected.is_empty() || !input.context.is_empty() {
        return false;
    }
    let normalized = input.query.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "你好" | "hello" | "hi" | "谢谢" | "thanks" | "早上好" | "晚上好" | "你是谁" | "你能做什么"
    )
}
