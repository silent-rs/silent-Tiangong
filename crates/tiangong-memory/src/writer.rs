//! Episode 写入器
//!
//! 负责从 TurnResult 提取 Episode 并写入 Memory 存储。
//! 优先使用 Memory 模型端点抽取结构化 Episode，失败时回退到规则提取。

use crate::types::{Episode, EpisodeOutcome, TurnResult};
use tiangong_llm::{LlmEndpointConfig, complete_text};

const EPISODE_WRITER_SYSTEM: &str = "\
你是独立记忆系统的 EpisodeWriter。根据一个 turn 的执行结果提取可长期保存的事件记忆。

要求：
- 只输出 JSON 对象，不要 Markdown，不要解释。
- title 用一句话概括事件，避免超过 40 个中文字符。
- summary 只保留未来回忆需要的信息，必须包含关键产物 URL、文件路径、重要工具结果；避免复述无用过程。
- outcome 可取 success、partial_success、failed、abandoned。
- keywords 只保留 3-10 个检索关键词。
- tool_calls 只保留本 turn 中实际有记忆价值的工具名，不要包含 recall_memory。
- importance 为 0.0 到 1.0；包含媒体 URL、文件产物、代码变更、关键决策时提高到 0.7 以上。

JSON 格式：
{
  \"title\": \"...\",
  \"summary\": \"...\",
  \"outcome\": \"success\",
  \"keywords\": [\"...\"],
  \"tool_calls\": [\"...\"],
  \"importance\": 0.8
}";

#[derive(Debug, Default, serde::Deserialize)]
struct EpisodeExtraction {
    title: Option<String>,
    summary: Option<String>,
    outcome: Option<String>,
    keywords: Option<Vec<String>>,
    tool_calls: Option<Vec<String>>,
    importance: Option<f32>,
}

/// 从 TurnResult 提取 Episode
///
/// 仅在 `turn_result.had_tool_calls == true` 时才生成 Episode。
pub(crate) fn extract_episode(turn_result: &TurnResult) -> Option<Episode> {
    if !turn_result.had_tool_calls && turn_result.artifacts.is_empty() {
        return None;
    }
    Some(extract_episode_fallback(turn_result))
}

pub(crate) async fn extract_episode_with_model(
    turn_result: &TurnResult,
    model: Option<&LlmEndpointConfig>,
) -> Option<Episode> {
    if !turn_result.had_tool_calls && turn_result.artifacts.is_empty() {
        return None;
    }
    let Some(model) = model else {
        return extract_episode(turn_result);
    };

    match extract_episode_with_model_inner(turn_result, model).await {
        Ok(episode) => Some(episode),
        Err(err) => {
            tracing::warn!("EpisodeWriter LLM 抽取失败，使用规则 fallback: {err}");
            extract_episode(turn_result)
        }
    }
}

async fn extract_episode_with_model_inner(
    turn_result: &TurnResult,
    model: &LlmEndpointConfig,
) -> anyhow::Result<Episode> {
    let prompt = build_writer_prompt(turn_result);
    let response = complete_text(model, EPISODE_WRITER_SYSTEM, &prompt, 900).await?;
    let json = extract_json_object(&response).unwrap_or(response.as_str());
    let extracted: EpisodeExtraction = serde_json::from_str(json)?;
    Ok(build_episode_from_extraction(turn_result, extracted))
}

fn extract_episode_fallback(turn_result: &TurnResult) -> Episode {
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

    Episode::new(
        turn_result.session_id.clone(),
        title,
        summary,
        outcome,
        keywords,
        turn_result.tool_calls.clone(),
        importance,
    )
}

fn build_episode_from_extraction(
    turn_result: &TurnResult,
    extracted: EpisodeExtraction,
) -> Episode {
    let fallback = extract_episode_fallback(turn_result);
    let title = extracted
        .title
        .map(|item| compact_text(&item, 80))
        .filter(|item| !item.is_empty())
        .unwrap_or(fallback.title);
    let summary = extracted
        .summary
        .map(|item| compact_text(&item, 1600))
        .filter(|item| !item.is_empty())
        .unwrap_or(fallback.summary);
    let keywords = extracted
        .keywords
        .map(dedupe_strings)
        .filter(|items| !items.is_empty())
        .unwrap_or(fallback.keywords);
    let mut tool_calls = turn_result.tool_calls.clone();
    if let Some(extracted_tool_calls) = extracted.tool_calls {
        tool_calls.extend(extracted_tool_calls);
    }
    let tool_calls = dedupe_strings(
        tool_calls
            .into_iter()
            .filter(|name| name != "recall_memory")
            .collect(),
    );
    let importance = extracted
        .importance
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(fallback.importance);
    Episode::new(
        turn_result.session_id.clone(),
        title,
        summary,
        parse_outcome(extracted.outcome.as_deref()).unwrap_or(fallback.outcome),
        keywords,
        tool_calls,
        importance,
    )
}

fn build_writer_prompt(turn_result: &TurnResult) -> String {
    let mut lines = vec![
        format!("session_id: {}", turn_result.session_id),
        format!("turn_id: {}", turn_result.turn_id),
        format!("had_tool_calls: {}", turn_result.had_tool_calls),
    ];
    if !turn_result.user_input.trim().is_empty() {
        lines.push(format!("user_input:\n{}", turn_result.user_input.trim()));
    }
    if !turn_result.summary.trim().is_empty() {
        lines.push(format!(
            "assistant_summary:\n{}",
            turn_result.summary.trim()
        ));
    }
    if !turn_result.tool_calls.is_empty() {
        lines.push(format!("tool_calls: {}", turn_result.tool_calls.join(", ")));
    }
    if !turn_result.artifacts.is_empty() {
        lines.push("artifacts:".to_string());
        for artifact in &turn_result.artifacts {
            lines.push(format!(
                "- kind={:?} tool={} title={} url={} path={} summary={}",
                artifact.kind,
                artifact.tool_name.as_deref().unwrap_or(""),
                artifact.title.as_deref().unwrap_or(""),
                artifact.url.as_deref().unwrap_or(""),
                artifact.path.as_deref().unwrap_or(""),
                artifact.summary.as_deref().unwrap_or("")
            ));
        }
    }
    lines.join("\n\n")
}

fn parse_outcome(raw: Option<&str>) -> Option<EpisodeOutcome> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "success" => Some(EpisodeOutcome::Success),
        "partial_success" | "partialsuccess" | "partial" => Some(EpisodeOutcome::PartialSuccess),
        "failed" | "failure" | "fail" => Some(EpisodeOutcome::Failed),
        "abandoned" | "cancelled" | "canceled" => Some(EpisodeOutcome::Abandoned),
        _ => None,
    }
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

fn dedupe_strings(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert(item.to_lowercase()))
        .collect()
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
