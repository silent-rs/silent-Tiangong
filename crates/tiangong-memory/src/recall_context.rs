//! Tool 化上下文回忆。
//!
//! Core 只把当前请求和最近语境传进来；Memory 内部自行规划检索锚点、
//! 调用召回、加载二跳内容，并输出去重后的增量信息。

use std::time::Instant;

use crate::llm_metrics::log_memory_llm_call;
use crate::recall_anchor::extract_recall_anchors;
use crate::store::MemoryStore;
use crate::types::{
    ExpandedMemory, MemoryKind, MemoryRecallRequest, MemoryRecallResponse, RecallDepth, RecallHit,
    SearchStrategy,
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

const DEEP_RECALL_DECISION_SYSTEM: &str = "\
你是独立 Memory 系统的深度回忆裁决器。

Core 传入的是一次外部刺激。你不能仅凭触发词决定深挖，必须基于初始回忆结果判断。

请只输出 JSON：
{
  \"need_deep_recall\": true/false,
  \"reason\": \"简短原因\",
  \"followup_queries\": [\"后续追溯查询\"],
  \"target_kinds\": [\"episode\", \"entity\", \"decision\", \"evidence\"],
  \"max_rounds\": 1
}

判断规则：
- 初始回忆已经足够回答时，need_deep_recall=false。
- 初始命中为空但刺激明显需要历史产物、历史决策或跨会话上下文时，可深挖。
- 命中 Entity/Decision 但缺少来源 Episode，或用户需要解释原因/来源时，可深挖。
- 命中产物但缺少生成上下文、保存路径或继续使用方式时，可深挖。
- followup_queries 最多 3 条，max_rounds 取 1~2。
- 如果初始命中已包含可追溯的 Entity/Decision，可将 followup_queries 留空，仅通过 target_kinds 指定要追溯的类型。
- 不要输出 JSON 之外的文字。";

#[derive(Debug, Clone, Default)]
struct DeepRecallDecision {
    need_deep_recall: bool,
    reason: String,
    followup_queries: Vec<String>,
    target_kinds: Vec<MemoryKind>,
    max_rounds: usize,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawDeepRecallDecision {
    need_deep_recall: Option<bool>,
    reason: Option<String>,
    #[serde(default)]
    followup_queries: Vec<String>,
    #[serde(default)]
    target_kinds: Vec<String>,
    max_rounds: Option<usize>,
}

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
            recall_depth: RecallDepth::Skip,
            usage: total_usage,
            deep_queries: Vec::new(),
        };
    }

    let initial_hits = dedupe_hits(store.recall_async(&plan.anchors, plan.limit).await);
    tracing::debug!(
        query = %request.query,
        hit_count = initial_hits.len(),
        "内存 recall 初始召回完成"
    );
    let initial_expanded = store.load_depth2(
        &initial_hits
            .iter()
            .map(|hit| hit.node_id.clone())
            .collect::<Vec<_>>(),
    );

    let (decision, decision_usage) =
        decide_deep_recall(model, &request, &initial_hits, &initial_expanded).await;
    if let Some(usage) = decision_usage {
        accumulate_usage(&mut total_usage, &usage);
    }

    let mut hits = initial_hits;
    let mut expanded = initial_expanded;
    let mut deep_queries = Vec::new();
    let mut recall_depth = infer_initial_depth(&hits);
    if decision.need_deep_recall && model.is_some() {
        tracing::debug!(
            query = %request.query,
            reason = %decision.reason,
            followup_count = decision.followup_queries.len(),
            "Memory deep recall 已触发"
        );
        let (deep_hits, deep_expanded, deep_usage, queries) =
            deep_recall(store, model, &request, &decision, &hits, &expanded).await;
        if let Some(usage) = deep_usage {
            accumulate_usage(&mut total_usage, &usage);
        }
        deep_queries = queries;
        if !deep_hits.is_empty() || !deep_expanded.is_empty() {
            recall_depth = RecallDepth::Deep;
            hits = merge_hits(hits, deep_hits);
            expanded = merge_expanded(expanded, deep_expanded);
        }
    }

    if hits.is_empty() {
        return MemoryRecallResponse {
            content: apply_output_budget(
                format!("未找到与「{}」相关的历史记忆。", request.query),
                DEFAULT_RECALL_OUTPUT_BUDGET_CHARS,
            ),
            hits,
            used_llm: plan.used_llm || model.is_some(),
            recall_depth,
            usage: total_usage,
            deep_queries,
        };
    }

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
        accumulate_usage(&mut total_usage, &usage);
    }
    let content =
        finalize_recall_content(&raw_content, &request, DEFAULT_RECALL_OUTPUT_BUDGET_CHARS);
    tracing::debug!(
        query = %request.query,
        content_chars = content.chars().count(),
        used_llm = plan.used_llm || model.is_some(),
        recall_depth = ?recall_depth,
        "内存 recall 输出整理完成"
    );

    MemoryRecallResponse {
        content,
        hits,
        used_llm: plan.used_llm || model.is_some(),
        recall_depth,
        deep_queries,
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

async fn decide_deep_recall(
    model: Option<&LlmEndpointConfig>,
    request: &MemoryRecallRequest,
    hits: &[RecallHit],
    expanded: &[ExpandedMemory],
) -> (DeepRecallDecision, Option<TokenUsageData>) {
    let Some(config) = model else {
        return (DeepRecallDecision::default(), None);
    };
    let prompt = format!(
        "外部刺激:\n{}\n\n调用原因:\n{}\n\n当前上下文:\n{}\n\n初始回忆结果:\n{}",
        request.query,
        request.reason.as_deref().unwrap_or(""),
        request.context.join("\n---\n"),
        format_candidates(hits, expanded),
    );
    let started = Instant::now();
    match complete_text_with_usage(config, DEEP_RECALL_DECISION_SYSTEM, &prompt, 512).await {
        Ok((text, usage)) => {
            log_memory_llm_call(
                "deep_recall_decision",
                config,
                started.elapsed(),
                usage.as_ref(),
            );
            let decision = parse_deep_recall_decision(&text).unwrap_or_else(|err| {
                tracing::warn!("Memory deep recall 裁决解析失败，跳过深挖: {err}");
                DeepRecallDecision::default()
            });
            (decision, usage)
        }
        Err(err) => {
            tracing::warn!("Memory deep recall 裁决失败，跳过深挖: {err}");
            (DeepRecallDecision::default(), None)
        }
    }
}

async fn deep_recall(
    store: &MemoryStore,
    model: Option<&LlmEndpointConfig>,
    request: &MemoryRecallRequest,
    decision: &DeepRecallDecision,
    initial_hits: &[RecallHit],
    initial_expanded: &[ExpandedMemory],
) -> (
    Vec<RecallHit>,
    Vec<ExpandedMemory>,
    Option<TokenUsageData>,
    Vec<String>,
) {
    let mut all_hits = Vec::new();
    let mut queries = Vec::new();
    let mut total_usage = TokenUsageData::default();
    let mut used_llm = false;
    let max_rounds = decision.max_rounds.clamp(1, 2);
    for query in decision
        .followup_queries
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .take(max_rounds * 3)
    {
        queries.push(query.to_string());
        let mut followup = request.clone();
        followup.query = query.to_string();
        followup.reason = Some(format!("deep recall: {}", decision.reason));
        let plan = extract_recall_anchors(model, &followup).await;
        if plan.used_llm {
            used_llm = true;
            accumulate_usage(&mut total_usage, &plan.usage);
        }
        let mut hits = store.recall_async(&plan.anchors, plan.limit).await;
        if !decision.target_kinds.is_empty() {
            hits.retain(|hit| {
                decision.target_kinds.contains(&hit.kind)
                    || matches!(hit.kind, MemoryKind::Entity | MemoryKind::Decision)
            });
        }
        all_hits.extend(hits);
    }

    let initial_ids = initial_hits
        .iter()
        .map(|hit| hit.node_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let hits = dedupe_hits(all_hits)
        .into_iter()
        .filter(|hit| !initial_ids.contains(hit.node_id.as_str()))
        .collect::<Vec<_>>();
    let expanded = store.load_depth2(
        &hits
            .iter()
            .map(|hit| hit.node_id.clone())
            .collect::<Vec<_>>(),
    );
    let seed_hits = merge_hits(initial_hits.to_vec(), hits.clone());
    let seed_expanded = merge_expanded(initial_expanded.to_vec(), expanded.clone());
    let (linked_hits, linked_expanded) =
        expand_related_memories(store, &seed_hits, &seed_expanded, max_rounds);
    tracing::debug!(
        query = %request.query,
        followup_queries = queries.len(),
        relation_hit_count = linked_hits.len(),
        "Memory deep recall 关系追溯完成"
    );
    (
        merge_hits(hits, linked_hits)
            .into_iter()
            .filter(|hit| !initial_ids.contains(hit.node_id.as_str()))
            .collect(),
        merge_expanded(expanded, linked_expanded),
        used_llm.then_some(total_usage),
        dedupe_strings(queries),
    )
}

fn expand_related_memories(
    store: &MemoryStore,
    seed_hits: &[RecallHit],
    seed_expanded: &[ExpandedMemory],
    max_rounds: usize,
) -> (Vec<RecallHit>, Vec<ExpandedMemory>) {
    let mut seen_ids = seed_hits
        .iter()
        .map(|hit| hit.node_id.clone())
        .chain(seed_expanded.iter().map(|item| item.node_id.clone()))
        .collect::<std::collections::HashSet<_>>();
    let mut all_hits = Vec::new();
    let mut all_expanded = Vec::new();
    let mut frontier = seed_expanded.to_vec();

    for round in 0..max_rounds.clamp(1, 2) {
        let frontier_node_ids = frontier
            .iter()
            .map(|item| item.node_id.clone())
            .collect::<Vec<_>>();
        let related_ids = related_node_ids_from_expanded(&frontier)
            .into_iter()
            .chain(store.related_node_ids(&frontier_node_ids))
            .filter(|id| seen_ids.insert(id.clone()))
            .collect::<Vec<_>>();
        if related_ids.is_empty() {
            break;
        }
        let hits = store.load_hits_by_ids(&related_ids);
        let hit_ids = hits
            .iter()
            .map(|hit| hit.node_id.clone())
            .collect::<Vec<_>>();
        let expanded = store.load_depth2(&hit_ids);
        tracing::debug!(
            round = round + 1,
            related_id_count = related_ids.len(),
            related_hit_count = hits.len(),
            "Memory deep recall 加载关联节点"
        );
        frontier = expanded.clone();
        all_hits = merge_hits(all_hits, hits);
        all_expanded = merge_expanded(all_expanded, expanded);
    }

    (all_hits, all_expanded)
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
    let started = Instant::now();
    let (text, usage) =
        complete_text_with_usage(config, RECALL_SYNTHESIS_SYSTEM, &prompt, 1200).await?;
    log_memory_llm_call(
        "recall_synthesis",
        config,
        started.elapsed(),
        usage.as_ref(),
    );
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

fn merge_hits(left: Vec<RecallHit>, right: Vec<RecallHit>) -> Vec<RecallHit> {
    dedupe_hits(left.into_iter().chain(right).collect())
}

fn merge_expanded(left: Vec<ExpandedMemory>, right: Vec<ExpandedMemory>) -> Vec<ExpandedMemory> {
    let mut seen = std::collections::HashSet::new();
    left.into_iter()
        .chain(right)
        .filter(|item| seen.insert(item.node_id.clone()))
        .collect()
}

fn infer_initial_depth(hits: &[RecallHit]) -> RecallDepth {
    if hits.is_empty() {
        return RecallDepth::Simple;
    }
    if hits.len() <= 2 && hits.iter().all(|hit| hit.score >= 0.7) {
        RecallDepth::Simple
    } else {
        RecallDepth::Normal
    }
}

fn accumulate_usage(total: &mut TokenUsageData, usage: &TokenUsageData) {
    total.prompt_tokens += usage.prompt_tokens;
    total.completion_tokens += usage.completion_tokens;
    total.total_tokens += usage.total_tokens;
}

fn parse_deep_recall_decision(text: &str) -> anyhow::Result<DeepRecallDecision> {
    let json = extract_json_object(text).unwrap_or(text);
    let raw: RawDeepRecallDecision = serde_json::from_str(json)?;
    let followup_queries = dedupe_strings(raw.followup_queries)
        .into_iter()
        .take(3)
        .collect::<Vec<_>>();
    let target_kinds = raw
        .target_kinds
        .into_iter()
        .filter_map(|item| parse_memory_kind(&item))
        .collect::<Vec<_>>();
    let has_trace_target = !followup_queries.is_empty() || !target_kinds.is_empty();
    Ok(DeepRecallDecision {
        need_deep_recall: raw.need_deep_recall.unwrap_or(false) && has_trace_target,
        reason: raw.reason.unwrap_or_default().trim().to_string(),
        followup_queries,
        target_kinds,
        max_rounds: raw.max_rounds.unwrap_or(1).clamp(1, 2),
    })
}

fn parse_memory_kind(text: &str) -> Option<MemoryKind> {
    match text.trim().to_ascii_lowercase().as_str() {
        "episode" => Some(MemoryKind::Episode),
        "entity" => Some(MemoryKind::Entity),
        "decision" => Some(MemoryKind::Decision),
        "evidence" => Some(MemoryKind::Evidence),
        _ => None,
    }
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then_some(&text[start..=end])
}

fn related_node_ids_from_expanded(expanded: &[ExpandedMemory]) -> Vec<String> {
    let mut ids = Vec::new();
    for item in expanded {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&item.full_content) else {
            continue;
        };
        collect_string_array(&value, "related_episodes", &mut ids);
        collect_string_array(&value, "episode_ids", &mut ids);
        collect_string_array(&value, "related_episode_ids", &mut ids);
    }
    dedupe_strings(ids)
}

fn collect_string_array(value: &serde_json::Value, key: &str, output: &mut Vec<String>) {
    let Some(items) = value.get(key).and_then(|item| item.as_array()) else {
        return;
    };
    output.extend(
        items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::to_string),
    );
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
                "{}. node_id={}\n类型: {:?}\n标题: {}\n摘要: {}\nscore: {:.2}\n完整内容:\n{}",
                idx + 1,
                hit.node_id,
                hit.kind,
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

    #[test]
    fn parse_deep_recall_decision_requires_followup_queries() {
        let decision = parse_deep_recall_decision(
            r#"{
                "need_deep_recall": true,
                "reason": "需要追溯来源 Episode",
                "followup_queries": ["why choose embedded vector", "why choose embedded vector"],
                "target_kinds": ["decision", "episode"],
                "max_rounds": 2
            }"#,
        )
        .unwrap();

        assert!(decision.need_deep_recall);
        assert_eq!(decision.followup_queries.len(), 1);
        assert_eq!(decision.target_kinds.len(), 2);
        assert_eq!(decision.max_rounds, 2);

        let no_query =
            parse_deep_recall_decision(r#"{"need_deep_recall": true, "reason": "missing query"}"#)
                .unwrap();
        assert!(!no_query.need_deep_recall);

        let relation_only = parse_deep_recall_decision(
            r#"{
                "need_deep_recall": true,
                "reason": "命中 Decision，需要追溯来源 Episode",
                "target_kinds": ["episode"]
            }"#,
        )
        .unwrap();
        assert!(relation_only.need_deep_recall);
        assert!(relation_only.followup_queries.is_empty());
    }

    #[test]
    fn related_node_ids_from_expanded_reads_entity_and_decision_links() {
        let ids = related_node_ids_from_expanded(&[
            ExpandedMemory {
                node_id: "entity-a".to_string(),
                full_content: serde_json::json!({
                    "id": "entity-a",
                    "related_episodes": ["ep-1", "ep-2", "ep-1"]
                })
                .to_string(),
            },
            ExpandedMemory {
                node_id: "decision-a".to_string(),
                full_content: serde_json::json!({
                    "id": "decision-a",
                    "episode_ids": ["ep-3"],
                    "related_episode_ids": ["ep-4"]
                })
                .to_string(),
            },
        ]);

        assert_eq!(ids, vec!["ep-1", "ep-2", "ep-3", "ep-4"]);
    }
}
