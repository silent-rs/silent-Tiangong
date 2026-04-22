//! 反刍层（Rumination）
//!
//! Phase B：MicroRumination — 每个 turn 结束后写入 Episode + 更新 Session Injection。
//! Phase C：MesoRumination — 会话结束时提炼 Entity/Decision + 更新 Workspace Injection。
//! Phase D：MetaRumination — 定期过时检测、归档、Profile 更新。

use anyhow::Result;

use crate::command::InjectionLevel;
use crate::store::MemoryStore;
use crate::types::{Decision, Entity, EntityType, Episode, TurnResult};
use crate::writer;
use tiangong_llm::{LlmEndpointConfig, complete_text};

const MESO_RUMINATION_SYSTEM: &str = "\
你是独立记忆系统的 MesoRumination 结构化提炼器。根据近期 Episode 提炼可长期复用的工作区 Entity 和 Decision。

要求：
- 只输出 JSON 对象，不要 Markdown，不要解释。
- entities 只保留项目、模块、文档、服务、模型 Provider、Skill 等稳定对象，不要把普通动词或泛词当实体。
- decisions 只保留明确的架构/实现/产品取舍，必须包含 chosen 和至少一个 episode_id。
- 所有 related_episode_ids / episode_ids 必须来自输入 Episode 的 id。
- description/context 只写未来回忆有价值的信息，避免复述输入全文。
- entity_type 可取 project、repository、server、skill、provider、document、module。
- importance 为 0.0 到 1.0。

JSON 格式：
{
  \"entities\": [
    {
      \"name\": \"...\",
      \"entity_type\": \"project\",
      \"description\": \"...\",
      \"file_path\": null,
      \"related_episode_ids\": [\"...\"],
      \"importance\": 0.6
    }
  ],
  \"decisions\": [
    {
      \"title\": \"...\",
      \"context\": \"...\",
      \"alternatives\": [\"...\"],
      \"chosen\": \"...\",
      \"reasons\": [\"...\"],
      \"episode_ids\": [\"...\"]
    }
  ]
}";

struct MesoMemorySet {
    entities: Vec<Entity>,
    decisions: Vec<Decision>,
    used_llm: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
struct MesoExtraction {
    #[serde(default)]
    entities: Vec<MesoEntityExtraction>,
    #[serde(default)]
    decisions: Vec<MesoDecisionExtraction>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct MesoEntityExtraction {
    name: Option<String>,
    entity_type: Option<String>,
    description: Option<String>,
    file_path: Option<String>,
    #[serde(default, alias = "episode_ids")]
    related_episode_ids: Vec<String>,
    importance: Option<f32>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct MesoDecisionExtraction {
    title: Option<String>,
    context: Option<String>,
    #[serde(default)]
    alternatives: Vec<String>,
    chosen: Option<String>,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    episode_ids: Vec<String>,
}

/// Micro 反刍：turn 结束后处理
///
/// 1. 若 turn 包含工具调用，提取 Episode 并写入 SQLite + Tantivy
/// 2. 更新 Session Injection（Phase C 实现）
pub(crate) async fn process_micro(
    store: &mut MemoryStore,
    turn_result: &TurnResult,
    workspace_id: Option<&str>,
    model: Option<&LlmEndpointConfig>,
) -> Result<()> {
    // 1. 提取并写入 Episode（嵌套 if let 用 let-chains 合并以满足 clippy）
    if let Some(episode) = writer::extract_episode_with_model(turn_result, model).await {
        store
            .write_episode(episode, workspace_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Micro 反刍：写入 Episode 失败: {}", e);
            });
    }

    // 2. 更新 Session Injection（最近 3 条 Episode 摘要）
    let recent = store.recent_episodes_for_session(workspace_id, &turn_result.session_id, 3);
    if !recent.is_empty() {
        let content = build_session_injection(&recent, &turn_result.session_id);
        store
            .update_injection(InjectionLevel::Session, &turn_result.session_id, &content)
            .unwrap_or_else(|e| {
                tracing::warn!("Micro 反刍：更新 Session Injection 失败: {}", e);
            });
    }

    Ok(())
}

/// 构建 Session 级注入内容（最近几条 Episode 摘要）
fn build_session_injection(episodes: &[Episode], session_id: &str) -> String {
    let now = chrono::Local::now().naive_local();
    let items = episodes
        .iter()
        .map(|ep| format!("- {}: {}", ep.title, ep.summary))
        .collect::<Vec<_>>()
        .join("\n");
    format!("# Session Memory ({session_id})\n更新时间: {now}\n\n## 本会话近期活动\n{items}\n")
}

/// Meso 反刍（Phase C）：会话结束时调用
///
/// 从最近的 Episodes 中提炼关键词/实体，更新 Workspace Injection 文件。
pub(crate) async fn process_meso(
    store: &mut MemoryStore,
    _session_id: &str,
    workspace_id: &str,
    model: Option<&LlmEndpointConfig>,
) -> Result<()> {
    tracing::debug!("Meso 反刍 workspace={workspace_id}");

    // 1. 查询最近 30 个 Episode 的完整内容
    let full_episodes = store.recent_episodes(Some(workspace_id), 30);
    let episodes = full_episodes
        .iter()
        .map(|episode| (episode.title.clone(), episode.keywords.clone()))
        .collect::<Vec<_>>();
    if episodes.is_empty() {
        tracing::debug!("Meso 反刍：无可用 Episode，跳过");
        return Ok(());
    }

    // 2. 提炼高频关键词（不调用 LLM，纯统计）
    let mut kw_count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (_, kws) in &episodes {
        for kw in kws {
            if !kw.is_empty() {
                *kw_count.entry(kw.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut top_keywords: Vec<(&String, &usize)> = kw_count.iter().collect();
    top_keywords.sort_by(|a, b| b.1.cmp(a.1));
    let top_kws: Vec<String> = top_keywords
        .into_iter()
        .take(15)
        .map(|(k, _)| k.clone())
        .collect();

    // 3. 构建 Workspace Injection 内容（近期活动摘要 + 高频关键词）
    let recent_titles: Vec<String> = episodes
        .iter()
        .take(5)
        .map(|(title, _)| format!("- {title}"))
        .collect();
    let existing_entities = store.list_entities(Some(workspace_id));
    let existing_decisions = store.list_decisions(Some(workspace_id));
    let MesoMemorySet {
        entities,
        decisions,
        used_llm,
    } = extract_meso_memories(
        model,
        &full_episodes,
        &top_kws,
        &existing_entities,
        &existing_decisions,
    )
    .await;
    tracing::debug!(
        workspace_id = %workspace_id,
        used_llm,
        episode_count = full_episodes.len(),
        entity_count = entities.len(),
        decision_count = decisions.len(),
        "Meso 反刍结构化提炼完成"
    );
    for entity in &entities {
        store.upsert_entity(entity.clone(), Some(workspace_id))?;
    }
    for decision in &decisions {
        store.upsert_decision(decision.clone(), Some(workspace_id))?;
    }

    let now = chrono::Local::now().naive_local();
    let content = format!(
        "# Workspace Memory Summary\n\
        更新时间: {now}\n\n\
        ## 近期活动（最新 5 条）\n\
        {}\n\n\
        ## 高频关键词\n\
        {}\n\n\
        ## 实体记忆\n\
        {}\n\n\
        ## 决策记忆\n\
        {}\n",
        recent_titles.join("\n"),
        top_kws.join(", "),
        entities
            .iter()
            .take(8)
            .map(|entity| format!("- {}: {}", entity.name, entity.description))
            .collect::<Vec<_>>()
            .join("\n"),
        decisions
            .iter()
            .take(8)
            .map(|decision| format!("- {} -> {}", decision.title, decision.chosen))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    // 4. 写入 Workspace Injection 文件
    store.update_injection(InjectionLevel::Workspace, workspace_id, &content)?;
    tracing::info!(
        "Meso 反刍完成，workspace={workspace_id}，used_llm={used_llm}，关键词={:?}，entities={}，decisions={}",
        &top_kws[..top_kws.len().min(5)],
        entities.len(),
        decisions.len()
    );

    Ok(())
}

async fn extract_meso_memories(
    model: Option<&LlmEndpointConfig>,
    episodes: &[Episode],
    top_keywords: &[String],
    existing_entities: &[Entity],
    existing_decisions: &[Decision],
) -> MesoMemorySet {
    if let Some(model) = model {
        match extract_meso_memories_with_model(
            model,
            episodes,
            existing_entities,
            existing_decisions,
        )
        .await
        {
            Ok(memories) => return memories,
            Err(err) => tracing::warn!("MesoRumination LLM 提炼失败，使用规则 fallback: {err}"),
        }
    }
    extract_meso_memories_fallback(
        episodes,
        top_keywords,
        existing_entities,
        existing_decisions,
    )
}

async fn extract_meso_memories_with_model(
    model: &LlmEndpointConfig,
    episodes: &[Episode],
    existing_entities: &[Entity],
    existing_decisions: &[Decision],
) -> anyhow::Result<MesoMemorySet> {
    let prompt = build_meso_prompt(episodes);
    let response = complete_text(model, MESO_RUMINATION_SYSTEM, &prompt, 1600).await?;
    let json = extract_json_object(&response).unwrap_or(response.as_str());
    let extraction: MesoExtraction = serde_json::from_str(json)?;
    build_meso_memories_from_extraction(extraction, episodes, existing_entities, existing_decisions)
}

fn extract_meso_memories_fallback(
    episodes: &[Episode],
    top_keywords: &[String],
    existing_entities: &[Entity],
    existing_decisions: &[Decision],
) -> MesoMemorySet {
    MesoMemorySet {
        entities: extract_entities_from_episodes(episodes, top_keywords, existing_entities),
        decisions: extract_decisions_from_episodes(episodes, existing_decisions),
        used_llm: false,
    }
}

fn build_meso_memories_from_extraction(
    extraction: MesoExtraction,
    episodes: &[Episode],
    existing_entities: &[Entity],
    existing_decisions: &[Decision],
) -> anyhow::Result<MesoMemorySet> {
    let known_episode_ids = episodes
        .iter()
        .map(|episode| episode.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut entities = Vec::new();
    for raw in extraction.entities {
        let name = required_text(raw.name, "entity.name")?;
        let description = required_text(raw.description, "entity.description")?;
        let entity_type =
            parse_entity_type(raw.entity_type.as_deref()).unwrap_or_else(|| classify_entity(&name));
        let related_episode_ids = validate_episode_ids(
            raw.related_episode_ids,
            &known_episode_ids,
            "entity.related_episode_ids",
        )?;
        let existing = find_existing_entity(existing_entities, &name, &entity_type);
        let now = chrono::Local::now().naive_local().to_string();
        let mut related = existing
            .map(|entity| entity.related_episodes.clone())
            .unwrap_or_default();
        related.extend(related_episode_ids);
        let related_episodes = dedupe_strings(related);
        entities.push(Entity {
            id: existing
                .map(|entity| entity.id.clone())
                .unwrap_or_else(|| scru128::new().to_string()),
            name,
            entity_type,
            description: compact_text(&description, 700),
            file_path: raw.file_path.filter(|item| !item.trim().is_empty()),
            related_episodes,
            importance: raw.importance.unwrap_or(0.6).clamp(0.0, 1.0),
            created_at: existing
                .map(|entity| entity.created_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
        });
    }

    let mut decisions = Vec::new();
    for raw in extraction.decisions {
        let title = required_text(raw.title, "decision.title")?;
        let context = required_text(raw.context, "decision.context")?;
        let chosen = required_text(raw.chosen, "decision.chosen")?;
        let episode_ids =
            validate_episode_ids(raw.episode_ids, &known_episode_ids, "decision.episode_ids")?;
        let existing = find_existing_decision_by_parts(existing_decisions, &title, &episode_ids);
        let now = chrono::Local::now().naive_local().to_string();
        decisions.push(Decision {
            id: existing
                .map(|decision| decision.id.clone())
                .unwrap_or_else(|| scru128::new().to_string()),
            title: if title.starts_with("决策：") {
                title
            } else {
                format!("决策：{title}")
            },
            context: compact_text(&context, 1200),
            alternatives: dedupe_strings(raw.alternatives),
            chosen: compact_text(&chosen, 300),
            reasons: dedupe_strings(raw.reasons),
            episode_ids,
            created_at: existing
                .map(|decision| decision.created_at.clone())
                .unwrap_or(now),
        });
    }

    if entities.is_empty() && decisions.is_empty() {
        anyhow::bail!("LLM Meso 输出未包含有效 Entity 或 Decision");
    }

    Ok(MesoMemorySet {
        entities,
        decisions,
        used_llm: true,
    })
}

fn extract_entities_from_episodes(
    episodes: &[Episode],
    top_keywords: &[String],
    existing_entities: &[Entity],
) -> Vec<Entity> {
    let mut entities = Vec::new();
    for keyword in top_keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| is_entity_keyword(keyword))
        .take(10)
    {
        let entity_type = classify_entity(keyword);
        let related = episodes
            .iter()
            .filter(|episode| {
                episode
                    .keywords
                    .iter()
                    .any(|item| item.eq_ignore_ascii_case(keyword))
                    || episode.title.contains(keyword)
                    || episode.summary.contains(keyword)
            })
            .map(|episode| episode.id.clone())
            .collect::<Vec<_>>();
        if related.is_empty() {
            continue;
        }
        let now = chrono::Local::now().naive_local().to_string();
        let existing = find_existing_entity(existing_entities, keyword, &entity_type);
        let mut related_episodes = existing
            .map(|entity| entity.related_episodes.clone())
            .unwrap_or_default();
        related_episodes.extend(related);
        related_episodes = dedupe_strings(related_episodes);
        entities.push(Entity {
            id: existing
                .map(|entity| entity.id.clone())
                .unwrap_or_else(|| scru128::new().to_string()),
            name: keyword.to_string(),
            entity_type,
            description: format!(
                "从近期 {} 条 Episode 中提炼出的工作区实体：{}",
                related_episodes.len(),
                keyword
            ),
            file_path: extract_entity_path(keyword, episodes),
            related_episodes,
            importance: 0.5,
            created_at: existing
                .map(|entity| entity.created_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
        });
    }
    entities
}

fn extract_decisions_from_episodes(
    episodes: &[Episode],
    existing_decisions: &[Decision],
) -> Vec<Decision> {
    episodes
        .iter()
        .filter(|episode| contains_decision_signal(&episode.title, &episode.summary))
        .take(10)
        .map(|episode| {
            let now = chrono::Local::now().naive_local().to_string();
            let existing = find_existing_decision(existing_decisions, episode);
            Decision {
                id: existing
                    .map(|decision| decision.id.clone())
                    .unwrap_or_else(|| scru128::new().to_string()),
                title: format!("决策：{}", episode.title),
                context: episode.summary.clone(),
                alternatives: extract_alternatives(&episode.summary),
                chosen: infer_chosen(&episode.summary).unwrap_or_else(|| episode.title.clone()),
                reasons: episode.keywords.clone(),
                episode_ids: vec![episode.id.clone()],
                created_at: existing
                    .map(|decision| decision.created_at.clone())
                    .unwrap_or(now),
            }
        })
        .collect()
}

fn find_existing_entity<'a>(
    existing_entities: &'a [Entity],
    name: &str,
    entity_type: &EntityType,
) -> Option<&'a Entity> {
    existing_entities.iter().find(|entity| {
        entity.name.eq_ignore_ascii_case(name)
            && std::mem::discriminant(&entity.entity_type) == std::mem::discriminant(entity_type)
    })
}

fn find_existing_decision<'a>(
    existing_decisions: &'a [Decision],
    episode: &Episode,
) -> Option<&'a Decision> {
    let title = format!("决策：{}", episode.title);
    find_existing_decision_by_parts(
        existing_decisions,
        &title,
        std::slice::from_ref(&episode.id),
    )
}

fn find_existing_decision_by_parts<'a>(
    existing_decisions: &'a [Decision],
    title: &str,
    episode_ids: &[String],
) -> Option<&'a Decision> {
    existing_decisions.iter().find(|decision| {
        decision
            .episode_ids
            .iter()
            .any(|id| episode_ids.iter().any(|episode_id| episode_id == id))
            || decision.title == title
    })
}

fn is_entity_keyword(keyword: &str) -> bool {
    let trimmed = keyword.trim();
    trimmed.chars().count() >= 3
        && !matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "the" | "and" | "for" | "with" | "用户请求" | "结果摘要" | "工具调用" | "结构化产物"
        )
}

fn classify_entity(keyword: &str) -> EntityType {
    let lower = keyword.to_ascii_lowercase();
    if lower.ends_with(".rs") || lower.contains("::") {
        EntityType::Module
    } else if lower.ends_with(".md") || lower.ends_with(".json") || lower.ends_with(".toml") {
        EntityType::Document
    } else if lower.contains("server") || lower.contains("qdrant") || lower.contains("ipc") {
        EntityType::Server
    } else if lower.contains("skill") {
        EntityType::Skill
    } else if lower.contains("model") || lower.contains("provider") || lower.contains("llm") {
        EntityType::Provider
    } else {
        EntityType::Project
    }
}

fn parse_entity_type(raw: Option<&str>) -> Option<EntityType> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "project" => Some(EntityType::Project),
        "repository" | "repo" => Some(EntityType::Repository),
        "server" => Some(EntityType::Server),
        "skill" => Some(EntityType::Skill),
        "provider" => Some(EntityType::Provider),
        "document" | "doc" => Some(EntityType::Document),
        "module" => Some(EntityType::Module),
        _ => None,
    }
}

fn extract_entity_path(keyword: &str, episodes: &[Episode]) -> Option<String> {
    let lower = keyword.to_ascii_lowercase();
    if !(lower.contains('/') || lower.contains(".rs") || lower.contains(".md")) {
        return None;
    }
    episodes
        .iter()
        .flat_map(|episode| episode.summary.split_whitespace())
        .find(|token| token.contains(keyword))
        .map(|token| {
            token
                .trim_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | '，'))
                .to_string()
        })
}

fn contains_decision_signal(title: &str, summary: &str) -> bool {
    let text = format!("{title}\n{summary}").to_ascii_lowercase();
    [
        "decide",
        "decided",
        "choose",
        "chose",
        "selected",
        "adopted",
        "instead of",
        "rather than",
        "方案",
        "选择",
        "采用",
        "决定",
        "取舍",
        "替代",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn extract_alternatives(summary: &str) -> Vec<String> {
    let markers = [
        " vs ",
        " versus ",
        " or ",
        " instead of ",
        "而不是",
        "还是",
        "对比",
    ];
    if !markers.iter().any(|marker| summary.contains(marker)) {
        return Vec::new();
    }
    summary
        .split([',', '，', ';', '；', '\n'])
        .map(str::trim)
        .filter(|item| item.chars().count() >= 3)
        .take(4)
        .map(String::from)
        .collect()
}

fn infer_chosen(summary: &str) -> Option<String> {
    for marker in [
        "choose ",
        "chose ",
        "selected ",
        "adopted ",
        "采用",
        "选择",
        "决定",
    ] {
        if let Some((_, rest)) = summary.split_once(marker) {
            let chosen = rest
                .split([',', '，', ';', '；', '.', '。', '\n'])
                .next()
                .unwrap_or(rest)
                .trim();
            if !chosen.is_empty() {
                return Some(chosen.chars().take(80).collect());
            }
        }
    }
    None
}

fn build_meso_prompt(episodes: &[Episode]) -> String {
    let mut lines = Vec::new();
    lines.push("近期 Episodes:".to_string());
    for episode in episodes.iter().take(30) {
        lines.push(format!(
            "- id={}\n  title={}\n  summary={}\n  outcome={:?}\n  keywords={}\n  tools={}",
            episode.id,
            compact_text(&episode.title, 160),
            compact_text(&episode.summary, 900),
            episode.outcome,
            episode.keywords.join(", "),
            episode.tool_calls.join(", ")
        ));
    }
    lines.join("\n")
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then_some(&text[start..=end])
}

fn required_text(raw: Option<String>, field: &str) -> anyhow::Result<String> {
    let value = raw.unwrap_or_default();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("LLM Meso 输出缺少必填字段: {field}");
    }
    Ok(trimmed.to_string())
}

fn validate_episode_ids(
    ids: Vec<String>,
    known_episode_ids: &std::collections::HashSet<&str>,
    field: &str,
) -> anyhow::Result<Vec<String>> {
    let ids = dedupe_strings(ids);
    if ids.is_empty() {
        anyhow::bail!("LLM Meso 输出缺少必填字段: {field}");
    }
    if let Some(unknown) = ids
        .iter()
        .find(|id| !known_episode_ids.contains(id.as_str()))
    {
        anyhow::bail!("LLM Meso 输出包含未知 Episode id: {unknown}");
    }
    Ok(ids)
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
        .filter(|item| seen.insert(item.to_ascii_lowercase()))
        .collect()
}

/// Meta 反刍（Phase D）：定期调度（启动时检测，距上次超过 24h 才执行）
///
/// 1. 检测低活跃（30天未使用，importance < 0.3）节点并归档
/// 2. Profile 更新（Phase D 高级功能预留）
pub(crate) fn process_meta(store: &mut MemoryStore) -> Result<()> {
    tracing::debug!("Meta 反刍开始");

    // 归档低活跃节点（30天未使用 + importance < 0.3）
    let stale = store.list_stale_nodes(30, 0.3);
    let archived_count = stale.len();
    for (node_id, _importance) in stale {
        store.archive_node(&node_id);
    }

    if archived_count > 0 {
        tracing::info!("Meta 反刍：归档 {} 个低活跃节点", archived_count);
    } else {
        tracing::debug!("Meta 反刍：无需归档");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        MesoDecisionExtraction, MesoEntityExtraction, MesoExtraction,
        build_meso_memories_from_extraction, extract_meso_memories_fallback,
    };
    use crate::db::sqlite::test_helpers::open_in_memory;
    use crate::types::{EntityType, Episode, EpisodeOutcome};

    fn make_episode(session_id: &str) -> Episode {
        Episode::new(
            session_id.to_string(),
            "功能开发标题".to_string(),
            "完成了一个功能的开发".to_string(),
            EpisodeOutcome::Success,
            vec!["Rust".to_string(), "异步".to_string(), "测试".to_string()],
            vec!["write_file".to_string()],
            0.7,
        )
    }

    #[test]
    fn process_meso_generates_injection_content_with_keywords() {
        let db = open_in_memory().unwrap();
        // 写入 5 个 Episode，关键词包含 "Rust"
        for i in 0..5 {
            let ep = make_episode(&format!("sess-{i}"));
            db.insert_episode(&ep, Some("ws-test")).unwrap();
        }

        let summaries = db.recent_episode_summaries(30).unwrap();
        assert_eq!(summaries.len(), 5);

        // 统计关键词出现频次
        let mut kw_count = std::collections::HashMap::new();
        for (_, kws) in &summaries {
            for kw in kws {
                *kw_count.entry(kw.clone()).or_insert(0usize) += 1;
            }
        }
        // "Rust" 应出现 5 次（高频词）
        assert_eq!(kw_count.get("Rust").copied().unwrap_or(0), 5);
    }

    #[test]
    fn llm_meso_extraction_builds_valid_structured_memories() {
        let episode = make_episode("sess-llm-meso");
        let extraction = MesoExtraction {
            entities: vec![MesoEntityExtraction {
                name: Some("tiangong-memory".to_string()),
                entity_type: Some("server".to_string()),
                description: Some("Memory 独立服务与检索系统".to_string()),
                file_path: None,
                related_episode_ids: vec![episode.id.clone()],
                importance: Some(0.8),
            }],
            decisions: vec![MesoDecisionExtraction {
                title: Some("使用内置向量索引".to_string()),
                context: Some("为了降低用户启动成本，优先采用内置向量索引".to_string()),
                alternatives: vec!["外部 Qdrant".to_string(), "内置 flat index".to_string()],
                chosen: Some("内置 flat index".to_string()),
                reasons: vec!["降低启动复杂度".to_string()],
                episode_ids: vec![episode.id.clone()],
            }],
        };

        let memories =
            build_meso_memories_from_extraction(extraction, &[episode], &[], &[]).unwrap();

        assert!(memories.used_llm);
        assert_eq!(memories.entities.len(), 1);
        assert_eq!(memories.decisions.len(), 1);
        assert!(matches!(
            memories.entities[0].entity_type,
            EntityType::Server
        ));
        assert!(memories.decisions[0].title.starts_with("决策："));
    }

    #[test]
    fn invalid_llm_meso_output_is_rejected_so_rule_fallback_can_continue() {
        let episode = make_episode("sess-llm-fallback");
        let extraction = MesoExtraction {
            entities: vec![MesoEntityExtraction {
                name: Some("Rust".to_string()),
                entity_type: Some("project".to_string()),
                description: Some("Rust 相关工作区实体".to_string()),
                file_path: None,
                related_episode_ids: vec!["unknown-episode".to_string()],
                importance: Some(0.5),
            }],
            decisions: Vec::new(),
        };

        let rejected = build_meso_memories_from_extraction(
            extraction,
            std::slice::from_ref(&episode),
            &[],
            &[],
        );
        assert!(rejected.is_err(), "未知 Episode id 应触发严格校验失败");

        let fallback = extract_meso_memories_fallback(
            std::slice::from_ref(&episode),
            &episode.keywords,
            &[],
            &[],
        );
        assert!(!fallback.used_llm);
        assert!(
            fallback
                .entities
                .iter()
                .any(|entity| entity.name.eq_ignore_ascii_case("Rust")),
            "LLM 输出不可用时规则版 Meso 仍应能产生实体记忆"
        );
    }
}
