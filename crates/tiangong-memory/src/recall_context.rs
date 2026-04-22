//! Tool 化上下文回忆。
//!
//! Core 只把当前请求和最近语境传进来；Memory 内部自行规划检索锚点、
//! 调用召回、加载二跳内容，并输出去重后的增量信息。

use crate::recall_anchor::extract_recall_anchors;
use crate::store::MemoryStore;
use crate::types::{
    ExpandedMemory, MemoryRecallRequest, MemoryRecallResponse, RecallHit, SearchStrategy,
};
use tiangong_llm::{LlmEndpointConfig, TokenUsageData, complete_text_with_usage};

const DEFAULT_RECALL_OUTPUT_BUDGET_CHARS: usize = 1200;

const RECALL_SYNTHESIS_SYSTEM: &str = "\
你是独立记忆系统的结果整理器。你的输出会被交给主模型继续推理。

要求：
- 只输出当前上下文中没有的新信息，避免复述用户问题、提示词或当前上下文已有内容。
- 合并重复命中；同一 URL、文件路径、node_id 只出现一次。
- 优先保留可执行线索：URL、文件路径、产物名称、决策结论、关键摘要。
- 不要输出泛泛解释，不要说“根据记忆”等套话。
- 如果没有增量信息，输出：没有发现当前上下文之外的增量记忆。
- 总长度控制在 1200 字以内。";

pub(crate) async fn recall_context(
    store: &MemoryStore,
    model: Option<&LlmEndpointConfig>,
    request: MemoryRecallRequest,
) -> MemoryRecallResponse {
    let request = normalize_request(request);
    if request.query.is_empty() {
        return MemoryRecallResponse {
            content: apply_output_budget(
                "recall_memory.query is empty".to_string(),
                DEFAULT_RECALL_OUTPUT_BUDGET_CHARS,
            ),
            ..MemoryRecallResponse::default()
        };
    }

    let plan = extract_recall_anchors(model, &request).await;
    let mut total_usage = plan.usage.clone();
    tracing::debug!(
        query = %request.query,
        strategy = ?plan.anchors.strategy,
        limit = plan.limit,
        used_llm = plan.used_llm,
        "内存 recall 规划完成"
    );
    if plan.anchors.strategy == Some(SearchStrategy::Skip) {
        return MemoryRecallResponse {
            content: apply_output_budget(
                "当前请求不需要检索长期记忆。".to_string(),
                DEFAULT_RECALL_OUTPUT_BUDGET_CHARS,
            ),
            hits: Vec::new(),
            used_llm: plan.used_llm,
            usage: total_usage,
        };
    }

    let hits = dedupe_hits(store.recall_async(&plan.anchors, plan.limit).await);
    tracing::debug!(
        query = %request.query,
        hit_count = hits.len(),
        "内存 recall 粗召回完成"
    );
    if hits.is_empty() {
        return MemoryRecallResponse {
            content: apply_output_budget(
                format!("未找到与「{}」相关的历史记忆。", request.query),
                DEFAULT_RECALL_OUTPUT_BUDGET_CHARS,
            ),
            hits,
            used_llm: plan.used_llm,
            usage: total_usage,
        };
    }

    let expanded = store.load_depth2(
        &hits
            .iter()
            .map(|hit| hit.node_id.clone())
            .collect::<Vec<_>>(),
    );
    let (raw_content, synthesis_usage) = match model {
        Some(config) => synthesize_with_model(config, &request, &hits, &expanded)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!("内存 recall 整理失败，使用规则 fallback: {err}");
                (fallback_synthesis(&request, &hits, &expanded), None)
            }),
        None => (fallback_synthesis(&request, &hits, &expanded), None),
    };
    if let Some(usage) = synthesis_usage {
        total_usage.prompt_tokens += usage.prompt_tokens;
        total_usage.completion_tokens += usage.completion_tokens;
        total_usage.total_tokens += usage.total_tokens;
    }
    let content =
        finalize_recall_content(&raw_content, &request, DEFAULT_RECALL_OUTPUT_BUDGET_CHARS);
    tracing::debug!(
        query = %request.query,
        content_chars = content.chars().count(),
        used_llm = plan.used_llm || model.is_some(),
        "内存 recall 输出整理完成"
    );

    MemoryRecallResponse {
        content,
        hits,
        used_llm: plan.used_llm || model.is_some(),
        usage: total_usage,
    }
}

fn normalize_request(mut request: MemoryRecallRequest) -> MemoryRecallRequest {
    request.query = request.query.trim().to_string();
    request.reason = request
        .reason
        .map(|reason| reason.trim().to_string())
        .filter(|reason| !reason.is_empty());
    request.expected = dedupe_strings(request.expected);
    request.context = request
        .context
        .into_iter()
        .map(|item| compact_text(&item, 800))
        .filter(|item| !item.is_empty())
        .take(30)
        .collect();
    request.limit = request.limit.clamp(1, 10);
    request
}

async fn synthesize_with_model(
    config: &LlmEndpointConfig,
    request: &MemoryRecallRequest,
    hits: &[RecallHit],
    expanded: &[ExpandedMemory],
) -> anyhow::Result<(String, Option<TokenUsageData>)> {
    let prompt = format!(
        "当前请求:\n{}\n\n调用原因:\n{}\n\n当前上下文（避免重复这些内容）:\n{}\n\n候选记忆:\n{}",
        request.query,
        request.reason.as_deref().unwrap_or(""),
        request.context.join("\n---\n"),
        format_candidates(hits, expanded),
    );
    let (text, usage) =
        complete_text_with_usage(config, RECALL_SYNTHESIS_SYSTEM, &prompt, 1200).await?;
    let compacted = compact_text(&text, DEFAULT_RECALL_OUTPUT_BUDGET_CHARS * 2);
    if compacted.is_empty() {
        Ok(("没有发现当前上下文之外的增量记忆。".to_string(), usage))
    } else {
        Ok((compacted, usage))
    }
}

fn fallback_synthesis(
    request: &MemoryRecallRequest,
    hits: &[RecallHit],
    expanded: &[ExpandedMemory],
) -> String {
    let context_text = request.context.join("\n");
    let mut seen = std::collections::HashSet::new();
    let mut emitted_urls = std::collections::HashSet::new();
    let mut emitted_paths = std::collections::HashSet::new();
    let mut emitted_tool_summaries = std::collections::HashSet::new();
    let mut lines = Vec::new();
    for hit in hits {
        if is_redundant(&hit.summary, &context_text) && is_redundant(&hit.title, &context_text) {
            continue;
        }
        let detail = expanded
            .iter()
            .find(|item| item.node_id == hit.node_id)
            .map(|item| item.full_content.as_str())
            .unwrap_or(hit.summary.as_str());
        let original_urls = extract_urls(detail);
        let original_paths = extract_paths(detail);
        if original_urls.is_empty()
            && original_paths.is_empty()
            && (is_redundant(&hit.summary, &context_text)
                || is_redundant(&hit.title, &context_text))
        {
            continue;
        }
        if !seen.insert(hit.node_id.clone()) {
            continue;
        }
        let urls = original_urls
            .iter()
            .filter(|url| emitted_urls.insert((*url).clone()))
            .cloned()
            .collect::<Vec<_>>();
        let paths = original_paths
            .iter()
            .filter(|path| emitted_paths.insert((*path).clone()))
            .cloned()
            .collect::<Vec<_>>();
        if urls.is_empty()
            && paths.is_empty()
            && (!original_urls.is_empty() || !original_paths.is_empty())
        {
            continue;
        }

        let cleaned_summary = strip_refs(&hit.summary, &original_urls, &original_paths);
        let tool_summary_key = normalize_for_redundancy(&cleaned_summary).to_ascii_lowercase();
        if urls.is_empty()
            && paths.is_empty()
            && !tool_summary_key.is_empty()
            && !emitted_tool_summaries.insert(tool_summary_key)
        {
            continue;
        }
        let mut item = format!(
            "- {}: {}",
            strip_refs(&hit.title, &original_urls, &original_paths),
            compact_text(&cleaned_summary, 240)
        );
        if !urls.is_empty() {
            item.push_str(&format!("\n  URLs: {}", urls.join(", ")));
        }
        if !paths.is_empty() {
            item.push_str(&format!("\n  paths: {}", paths.join(", ")));
        }
        lines.push(item);
    }

    if lines.is_empty() {
        "没有发现当前上下文之外的增量记忆。".to_string()
    } else {
        lines.join("\n")
    }
}

fn strip_refs(text: &str, urls: &[String], paths: &[String]) -> String {
    let mut cleaned = text.to_string();
    for item in urls.iter().chain(paths.iter()) {
        cleaned = cleaned.replace(item, "");
    }
    compact_text(
        &cleaned
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace("  ", " "),
        240,
    )
}

fn dedupe_hits(hits: Vec<RecallHit>) -> Vec<RecallHit> {
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .filter(|hit| seen.insert(hit.node_id.clone()))
        .collect()
}

fn dedupe_strings(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert(item.to_lowercase()))
        .collect()
}

fn format_candidates(hits: &[RecallHit], expanded: &[ExpandedMemory]) -> String {
    hits.iter()
        .enumerate()
        .map(|(idx, hit)| {
            let detail = expanded
                .iter()
                .find(|item| item.node_id == hit.node_id)
                .map(|item| compact_text(&item.full_content, 1200))
                .unwrap_or_default();
            format!(
                "{}. node_id={}\n标题: {}\n摘要: {}\nscore: {:.2}\n完整内容:\n{}",
                idx + 1,
                hit.node_id,
                hit.title,
                hit.summary,
                hit.score,
                detail
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut clipped = normalized.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}

fn finalize_recall_content(
    content: &str,
    request: &MemoryRecallRequest,
    budget_chars: usize,
) -> String {
    let context_text = request.context.join("\n");
    let mut seen_lines = std::collections::HashSet::new();
    let mut emitted_urls = std::collections::HashSet::new();
    let mut emitted_paths = std::collections::HashSet::new();
    let mut lines = Vec::new();

    for line in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if is_redundant(line, &context_text) {
            continue;
        }
        let urls = extract_urls(line);
        let paths = extract_paths(line);
        if urls
            .iter()
            .any(|url| !emitted_urls.insert(url.to_ascii_lowercase()))
            || paths
                .iter()
                .any(|path| !emitted_paths.insert(path.to_ascii_lowercase()))
        {
            continue;
        }
        let key = normalize_for_redundancy(line).to_ascii_lowercase();
        if seen_lines.insert(key) {
            lines.push(line.to_string());
        }
    }

    let cleaned = if lines.is_empty() {
        "没有发现当前上下文之外的增量记忆。".to_string()
    } else {
        lines.join("\n")
    };
    apply_output_budget(cleaned, budget_chars)
}

fn apply_output_budget(content: String, budget_chars: usize) -> String {
    if content.chars().count() <= budget_chars {
        return content;
    }
    let mut clipped = content
        .chars()
        .take(budget_chars.saturating_sub(3))
        .collect::<String>();
    clipped.push_str("...");
    clipped
}

fn is_redundant(text: &str, context: &str) -> bool {
    let text = normalize_for_redundancy(text);
    if text.chars().count() < 12 {
        return false;
    }
    if context.contains(&text) {
        return true;
    }
    context
        .lines()
        .map(strip_role_prefix)
        .map(normalize_for_redundancy)
        .filter(|item| item.chars().count() >= 12)
        .any(|item| text.contains(&item))
}

fn strip_role_prefix(text: &str) -> &str {
    let Some((prefix, rest)) = text.split_once(':') else {
        return text;
    };
    match prefix.trim().to_ascii_lowercase().as_str() {
        "user" | "assistant" | "system" | "tool" => rest.trim(),
        _ => text,
    }
}

fn normalize_for_redundancy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for prefix in ["https://", "http://", "data:image/"] {
        let mut start = 0;
        while let Some(offset) = text[start..].find(prefix) {
            let url_start = start + offset;
            let rest = &text[url_start..];
            let url_end = rest
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | '，' | '。' | '\\')
                })
                .unwrap_or(rest.len());
            let url = rest[..url_end].trim_matches(|c: char| {
                matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | '，' | '。')
            });
            if !url.is_empty() {
                urls.push(url.to_string());
            }
            start = url_start + url_end;
        }
    }
    dedupe_strings(urls)
}

fn extract_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for token in text.split_whitespace() {
        let cleaned = token
            .trim_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | '，' | '。'));
        if cleaned.contains("http://") || cleaned.contains("https://") || cleaned.contains("data:")
        {
            continue;
        }
        let cleaned = cleaned.strip_prefix("path=").unwrap_or(cleaned);
        let cleaned = cleaned
            .split(['"', '\'', ')', ']', '}', ',', '，', '。'])
            .next()
            .unwrap_or(cleaned);
        if cleaned.starts_with('/')
            || cleaned.starts_with("./")
            || cleaned.starts_with("../")
            || cleaned.contains(".rs")
            || cleaned.contains(".md")
            || cleaned.contains(".png")
            || cleaned.contains(".jpg")
            || cleaned.contains(".mp4")
        {
            paths.push(cleaned.to_string());
        }
    }
    dedupe_strings(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryKind;

    fn hit(node_id: &str, title: &str, summary: &str) -> RecallHit {
        RecallHit {
            node_id: node_id.to_string(),
            title: title.to_string(),
            summary: summary.to_string(),
            score: 1.0,
            kind: MemoryKind::Episode,
            importance: 0.5,
            depth1_loaded: false,
        }
    }

    #[test]
    fn dedupe_hits_uses_node_id_only() {
        let hits = dedupe_hits(vec![
            hit("node-a", "title", "summary one"),
            hit("node-a", "title", "summary two"),
            hit("node-b", "title", "summary two"),
        ]);

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].node_id, "node-a");
        assert_eq!(hits[1].node_id, "node-b");
    }

    #[test]
    fn finalize_recall_content_applies_budget_and_dedupes_refs() {
        let request = MemoryRecallRequest {
            query: "continue artifact".to_string(),
            context: vec!["assistant: 已经知道 repeated context line".to_string()],
            ..MemoryRecallRequest::default()
        };
        let content = "\
repeated context line
- first url https://example.invalid/a.png
- duplicate url https://example.invalid/a.png
- first path /tmp/a.png
- duplicate path /tmp/a.png
- keep this new detail";

        let finalized = finalize_recall_content(content, &request, 90);

        assert!(!finalized.contains("repeated context line"));
        assert_eq!(
            finalized.matches("https://example.invalid/a.png").count(),
            1
        );
        assert_eq!(finalized.matches("/tmp/a.png").count(), 1);
        assert!(finalized.chars().count() <= 90);
    }

    #[test]
    fn fallback_synthesis_dedupes_tool_result_summaries() {
        let request = MemoryRecallRequest {
            query: "tool result".to_string(),
            ..MemoryRecallRequest::default()
        };
        let hits = vec![
            hit("node-a", "tool result a", "same tool output summary"),
            hit("node-b", "tool result b", "same tool output summary"),
        ];

        let content = fallback_synthesis(&request, &hits, &[]);

        assert_eq!(content.matches("same tool output summary").count(), 1);
    }
}
