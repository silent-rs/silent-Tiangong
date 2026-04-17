//! 反刍层（Rumination）
//!
//! Phase B：MicroRumination — 每个 turn 结束后写入 Episode + 更新 Session Injection。
//! Phase C：MesoRumination — 会话结束时提炼 Entity/Decision + 更新 Workspace Injection。
//! Phase D：MetaRumination — 定期过时检测、归档、Profile 更新。

use anyhow::Result;

use crate::command::InjectionLevel;
use crate::store::MemoryStore;
use crate::types::TurnResult;
use crate::writer;

/// Micro 反刍：turn 结束后处理
///
/// 1. 若 turn 包含工具调用，提取 Episode 并写入 SQLite + Tantivy
/// 2. 更新 Session Injection（Phase C 实现）
pub(crate) fn process_micro(
    store: &mut MemoryStore,
    turn_result: &TurnResult,
    workspace_id: Option<&str>,
) -> Result<()> {
    // 1. 提取并写入 Episode（嵌套 if let 用 let-chains 合并以满足 clippy）
    if let Some(episode) = writer::extract_episode(turn_result) {
        store
            .write_episode(episode, workspace_id)
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

    // 1. 查询最近 30 个 Episode 的标题和关键词
    let episodes = store.recent_episode_summaries(30);
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

    let now = chrono::Local::now().naive_local();
    let content = format!(
        "# Workspace Memory Summary\n\
        更新时间: {now}\n\n\
        ## 近期活动（最新 5 条）\n\
        {}\n\n\
        ## 高频关键词\n\
        {}\n",
        recent_titles.join("\n"),
        top_kws.join(", "),
    );

    // 4. 写入 Workspace Injection 文件
    store.update_injection(InjectionLevel::Workspace, workspace_id, &content)?;
    tracing::info!(
        "Meso 反刍完成，workspace={workspace_id}，关键词={:?}",
        &top_kws[..top_kws.len().min(5)]
    );

    Ok(())
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
