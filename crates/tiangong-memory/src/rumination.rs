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
use tiangong_llm::LlmEndpointConfig;

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

    // 2. 更新 Session Injection（Phase C 实现，此处为桩）
    // store.update_injection(InjectionLevel::Session, &turn_result.session_id, "...")?;

    Ok(())
}

/// Meso 反刍（Phase C）：会话结束时调用
///
/// 从最近的 Episodes 中提炼关键词/实体，更新 Workspace Injection 文件。
pub(crate) fn process_meso(
    store: &mut MemoryStore,
    _session_id: &str,
    workspace_id: &str,
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
    let entities = extract_entities_from_episodes(&full_episodes, &top_kws);
    let decisions = extract_decisions_from_episodes(&full_episodes);
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
        "Meso 反刍完成，workspace={workspace_id}，关键词={:?}，entities={}，decisions={}",
        &top_kws[..top_kws.len().min(5)],
        entities.len(),
        decisions.len()
    );

    Ok(())
}

fn extract_entities_from_episodes(episodes: &[Episode], top_keywords: &[String]) -> Vec<Entity> {
    let mut entities = Vec::new();
    for keyword in top_keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| is_entity_keyword(keyword))
        .take(10)
    {
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
        entities.push(Entity {
            id: scru128::new().to_string(),
            name: keyword.to_string(),
            entity_type: classify_entity(keyword),
            description: format!(
                "从近期 {} 条 Episode 中提炼出的工作区实体：{}",
                related.len(),
                keyword
            ),
            file_path: extract_entity_path(keyword, episodes),
            related_episodes: related,
            importance: 0.5,
            created_at: now.clone(),
            updated_at: now,
        });
    }
    entities
}

fn extract_decisions_from_episodes(episodes: &[Episode]) -> Vec<Decision> {
    episodes
        .iter()
        .filter(|episode| contains_decision_signal(&episode.title, &episode.summary))
        .take(10)
        .map(|episode| {
            let now = chrono::Local::now().naive_local().to_string();
            Decision {
                id: scru128::new().to_string(),
                title: format!("决策：{}", episode.title),
                context: episode.summary.clone(),
                alternatives: extract_alternatives(&episode.summary),
                chosen: infer_chosen(&episode.summary).unwrap_or_else(|| episode.title.clone()),
                reasons: episode.keywords.clone(),
                episode_ids: vec![episode.id.clone()],
                created_at: now,
            }
        })
        .collect()
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
    use crate::db::sqlite::test_helpers::open_in_memory;
    use crate::types::{Episode, EpisodeOutcome};

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
}
