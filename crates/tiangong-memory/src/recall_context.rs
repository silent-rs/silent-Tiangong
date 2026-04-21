//! Tool 化上下文回忆。
//!
//! Core 只把当前请求和最近语境传进来；Memory 内部自行规划检索锚点、
//! 调用召回、加载二跳内容，并输出去重后的增量信息。

use crate::store::MemoryStore;
use crate::types::{
    ExpandedMemory, MemoryRecallRequest, MemoryRecallResponse, RecallAnchors, RecallHit,
    SearchStrategy,
};
use tiangong_llm::{LlmEndpointConfig, complete_text};

const RECALL_PLAN_SYSTEM: &str = "\
你是独立记忆系统的检索规划器。根据当前请求和语境，生成适合长期记忆检索的锚点。

要求：
- 只输出 JSON 对象，不要 Markdown。
- query 应改写为面向历史记忆检索的短查询，不要照抄整段提示词。
- keywords 只保留能区分历史记录的实体、文件名、工具名、媒体/产物类型。
- strategy 可取 keyword、semantic、hybrid。
- 用户提到刚刚、刚才、之前、上次、继续、那个、这张图、生成的图片等历史指代时，优先 semantic 或 hybrid。

JSON 格式：
{
  \"query\": \"...\",
  \"keywords\": [\"...\"],
  \"strategy\": \"hybrid\",
  \"semantic_ratio\": 0.7
}";

const RECALL_SYNTHESIS_SYSTEM: &str = "\
你是独立记忆系统的结果整理器。你的输出会被交给主模型继续推理。

要求：
- 只输出当前上下文中没有的新信息，避免复述用户问题、提示词或当前上下文已有内容。
- 合并重复命中；同一 URL、文件路径、node_id 只出现一次。
- 优先保留可执行线索：URL、文件路径、产物名称、决策结论、关键摘要。
- 不要输出泛泛解释，不要说“根据记忆”等套话。
- 如果没有增量信息，输出：没有发现当前上下文之外的增量记忆。
- 总长度控制在 1200 字以内。";

#[derive(Debug, Default, serde::Deserialize)]
struct RecallPlan {
    query: Option<String>,
    keywords: Option<Vec<String>>,
    strategy: Option<String>,
    semantic_ratio: Option<f64>,
    limit: Option<usize>,
}

pub(crate) async fn recall_context(
    store: &MemoryStore,
    model: Option<&LlmEndpointConfig>,
    request: MemoryRecallRequest,
) -> MemoryRecallResponse {
    let request = normalize_request(request);
    if request.query.is_empty() {
        return MemoryRecallResponse {
            content: "recall_memory.query is empty".to_string(),
            ..MemoryRecallResponse::default()
        };
    }

    let plan = match model {
        Some(config) => plan_with_model(config, &request)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!("Memory recall 规划失败，使用规则 fallback: {err}");
                fallback_plan(&request)
            }),
        None => fallback_plan(&request),
    };

    let limit = plan.limit.unwrap_or(request.limit).clamp(1, 10);
    let anchors = RecallAnchors {
        query: plan.query.clone().unwrap_or_else(|| request.query.clone()),
        keywords: plan.keywords.clone().unwrap_or_default(),
        strategy: plan.strategy.clone(),
    };

    let hits = dedupe_hits(store.recall_async(&anchors, limit).await);
    if hits.is_empty() {
        return MemoryRecallResponse {
            content: format!("未找到与「{}」相关的历史记忆。", request.query),
            hits,
            used_llm: model.is_some(),
        };
    }

    let expanded = store.load_depth2(
        &hits
            .iter()
            .map(|hit| hit.node_id.clone())
            .collect::<Vec<_>>(),
    );
    let content = match model {
        Some(config) => synthesize_with_model(config, &request, &hits, &expanded)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!("Memory recall 整理失败，使用规则 fallback: {err}");
                fallback_synthesis(&request, &hits, &expanded)
            }),
        None => fallback_synthesis(&request, &hits, &expanded),
    };

    MemoryRecallResponse {
        content,
        hits,
        used_llm: model.is_some(),
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

async fn plan_with_model(
    config: &LlmEndpointConfig,
    request: &MemoryRecallRequest,
) -> anyhow::Result<PlannedAnchors> {
    let prompt = format!(
        "当前请求:\n{}\n\n调用原因:\n{}\n\n期望内容:\n{}\n\n最近语境:\n{}",
        request.query,
        request.reason.as_deref().unwrap_or(""),
        request.expected.join(", "),
        request.context.join("\n---\n")
    );
    let response = complete_text(config, RECALL_PLAN_SYSTEM, &prompt, 512).await?;
    let json = extract_json_object(&response).unwrap_or(response.as_str());
    let parsed: RecallPlan = serde_json::from_str(json)?;
    Ok(PlannedAnchors {
        query: parsed
            .query
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty()),
        keywords: parsed.keywords.map(dedupe_strings),
        strategy: parse_strategy(parsed.strategy.as_deref(), parsed.semantic_ratio),
        limit: parsed.limit,
    })
}

async fn synthesize_with_model(
    config: &LlmEndpointConfig,
    request: &MemoryRecallRequest,
    hits: &[RecallHit],
    expanded: &[ExpandedMemory],
) -> anyhow::Result<String> {
    let prompt = format!(
        "当前请求:\n{}\n\n调用原因:\n{}\n\n当前上下文（避免重复这些内容）:\n{}\n\n候选记忆:\n{}",
        request.query,
        request.reason.as_deref().unwrap_or(""),
        request.context.join("\n---\n"),
        format_candidates(hits, expanded),
    );
    let text = complete_text(config, RECALL_SYNTHESIS_SYSTEM, &prompt, 1200).await?;
    let compacted = compact_text(&text, 3000);
    if compacted.is_empty() {
        Ok("没有发现当前上下文之外的增量记忆。".to_string())
    } else {
        Ok(compacted)
    }
}

#[derive(Debug, Default)]
struct PlannedAnchors {
    query: Option<String>,
    keywords: Option<Vec<String>>,
    strategy: Option<SearchStrategy>,
    limit: Option<usize>,
}

fn fallback_plan(request: &MemoryRecallRequest) -> PlannedAnchors {
    let mut keywords = request.expected.clone();
    keywords.extend(
        request
            .query
            .split(|c: char| c.is_whitespace() || "，。！？；：、,.!?;:'\"()（）".contains(c))
            .map(str::trim)
            .filter(|item| item.chars().count() >= 2)
            .take(8)
            .map(String::from),
    );
    let strategy = if contains_history_reference(&request.query) {
        SearchStrategy::Semantic
    } else {
        SearchStrategy::Hybrid {
            semantic_ratio: 0.6,
        }
    };
    PlannedAnchors {
        query: Some(request.query.clone()),
        keywords: Some(dedupe_strings(keywords)),
        strategy: Some(strategy),
        limit: Some(request.limit),
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
        .filter(|hit| {
            let key = format!(
                "{}:{}",
                hit.node_id,
                compact_text(&hit.summary.to_lowercase(), 120)
            );
            seen.insert(key)
        })
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

fn parse_strategy(raw: Option<&str>, semantic_ratio: Option<f64>) -> Option<SearchStrategy> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "keyword" => Some(SearchStrategy::Keyword),
        "semantic" => Some(SearchStrategy::Semantic),
        "hybrid" => Some(SearchStrategy::Hybrid {
            semantic_ratio: semantic_ratio.unwrap_or(0.6).clamp(0.0, 1.0),
        }),
        _ => None,
    }
}

fn contains_history_reference(text: &str) -> bool {
    [
        "刚刚",
        "刚才",
        "之前",
        "上次",
        "继续",
        "那个",
        "这张图",
        "生成的图片",
        "previous",
        "earlier",
        "that one",
    ]
    .iter()
    .any(|marker| text.contains(marker))
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

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then_some(&text[start..=end])
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
