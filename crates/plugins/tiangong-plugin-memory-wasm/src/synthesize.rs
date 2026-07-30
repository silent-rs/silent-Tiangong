//! 召回结果规则整理（自 `tiangong-memory` 下沉的 fallback 路径）。
//!
//! 对应宿主侧 `recall_context.rs` 的 `fallback_synthesis` 简化版：
//! 去除与当前上下文重复的命中、提取并去重 URL/路径、按命中重要性拼装文本。

use std::collections::HashSet;

use crate::bindings::exports::tiangong::plugin::plugin::RecallHit;
use crate::text_utils::{extract_paths, extract_urls};

/// 规则整理召回结果为文本。
///
/// - 跳过标题与摘要都已在当前上下文中出现的命中
/// - 提取每条命中的 URL/路径，全局去重
/// - 按重要性排序后拼装成列表
pub(crate) fn fallback_synthesize(query: &str, context: &[String], hits: &[RecallHit]) -> String {
    let context_text = context.join("\n");
    let mut seen_nodes = HashSet::new();
    let mut emitted_urls: HashSet<String> = HashSet::new();
    let mut emitted_paths: HashSet<String> = HashSet::new();
    let mut lines: Vec<String> = Vec::new();

    // 按重要性降序处理。
    let mut ordered: Vec<&RecallHit> = hits.iter().collect();
    ordered.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for hit in ordered {
        if !seen_nodes.insert(hit.node_id.clone()) {
            continue;
        }
        // 跳过标题与摘要都已在上下文出现的命中。
        if is_redundant(&hit.summary, &context_text) && is_redundant(&hit.title, &context_text) {
            continue;
        }

        let urls: Vec<String> = extract_urls(&hit.summary)
            .into_iter()
            .filter(|u| emitted_urls.insert(u.clone()))
            .collect();
        let paths: Vec<String> = extract_paths(&hit.summary)
            .into_iter()
            .filter(|p| emitted_paths.insert(p.clone()))
            .collect();

        let mut item = format!("- {}: {}", hit.title, hit.summary.trim());
        if !urls.is_empty() {
            item.push_str(&format!("\n  相关链接: {}", urls.join(" ")));
        }
        if !paths.is_empty() {
            item.push_str(&format!("\n  相关文件: {}", paths.join(" ")));
        }
        lines.push(item);
    }

    if lines.is_empty() {
        return format!("未在记忆中找到与「{query}」相关的历史上下文（规则整理无命中）。");
    }

    let mut out = format!("已回忆与「{query}」相关的历史上下文：\n");
    out.push_str(&lines.join("\n"));
    out
}

/// 判断文本是否已（归一化后）包含在上下文中。
fn is_redundant(text: &str, context: &str) -> bool {
    let n_text = normalize_for_redundancy(text);
    n_text.is_empty() || context.contains(&n_text)
}

fn normalize_for_redundancy(text: &str) -> String {
    text.trim().to_lowercase()
}
