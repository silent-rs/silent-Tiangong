//! 召回结果融合与重排（自 `tiangong-memory` 下沉的纯算法）。
//!
//! 将 BM25 与语义召回结果融合、归一化、重排。纯逻辑，无 IO、无 LLM。
//! 与宿主侧 `search/reranker.rs` 保持一致。

use std::collections::HashMap;

use crate::bindings::exports::tiangong::plugin::plugin::{MemoryKind, RecallHit};

/// 融合重排器。
pub(crate) struct Reranker {
    bm25_weight: f64,
    semantic_weight: f64,
}

impl Reranker {
    pub(crate) fn new(bm25_weight: f64, semantic_weight: f64) -> Self {
        Self {
            bm25_weight,
            semantic_weight,
        }
    }

    /// 根据语义倾向比率创建 Reranker（0.0=偏关键词 .. 1.0=偏语义）。
    pub(crate) fn from_semantic_ratio(s: f64) -> Self {
        let bm25_w = 0.7 - 0.4 * s;
        let sem_w = 0.3 + 0.4 * s;
        Self::new(bm25_w, sem_w)
    }

    /// 融合两路召回结果并返回 topK。
    ///
    /// - 分数归一化：min-max
    /// - 双命中奖励：同时出现在两路结果中加 0.2
    /// - 重要度加权（±10%）
    pub(crate) fn fuse(
        &self,
        bm25: Vec<RecallHit>,
        semantic: Vec<RecallHit>,
        limit: usize,
    ) -> Vec<RecallHit> {
        let bm25_norm = normalize_scores(bm25);
        let semantic_norm = normalize_scores(semantic);

        let mut fused: HashMap<String, FusedEntry> = HashMap::new();

        for hit in &bm25_norm {
            let entry = fused
                .entry(hit.node_id.clone())
                .or_insert_with(|| FusedEntry::from_hit(hit));
            entry.bm25_score = Some(hit.score);
        }
        for hit in &semantic_norm {
            let entry = fused
                .entry(hit.node_id.clone())
                .or_insert_with(|| FusedEntry::from_hit(hit));
            entry.semantic_score = Some(hit.score);
        }

        let bm25_w = self.bm25_weight;
        let semantic_w = self.semantic_weight;
        let mut results: Vec<RecallHit> = fused
            .into_values()
            .map(|entry| {
                let bm25_s = entry.bm25_score.unwrap_or(0.0);
                let sem_s = entry.semantic_score.unwrap_or(0.0);
                let dual_bonus = if entry.bm25_score.is_some() && entry.semantic_score.is_some() {
                    0.2
                } else {
                    0.0
                };
                let fused_score = bm25_s * bm25_w + sem_s * semantic_w + dual_bonus;
                let final_score = fused_score * (0.9 + entry.importance * 0.1);
                RecallHit {
                    node_id: entry.node_id,
                    title: entry.title,
                    summary: entry.summary,
                    score: final_score,
                    importance: entry.importance,
                    depth1_loaded: entry.depth1_loaded,
                    kind: entry.kind,
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }
}

/// Min-Max 归一化，将分数归一化到 [0, 1]。
fn normalize_scores(hits: Vec<RecallHit>) -> Vec<RecallHit> {
    if hits.is_empty() {
        return hits;
    }
    let min = hits.iter().map(|h| h.score).fold(f64::MAX, f64::min);
    let max = hits.iter().map(|h| h.score).fold(f64::MIN, f64::max);
    let range = max - min;
    if range < 1e-10 {
        return hits;
    }
    hits.into_iter()
        .map(|mut h| {
            h.score = (h.score - min) / range;
            h
        })
        .collect()
}

struct FusedEntry {
    node_id: String,
    title: String,
    summary: String,
    importance: f64,
    depth1_loaded: bool,
    kind: MemoryKind,
    bm25_score: Option<f64>,
    semantic_score: Option<f64>,
}

impl FusedEntry {
    fn from_hit(hit: &RecallHit) -> Self {
        Self {
            node_id: hit.node_id.clone(),
            title: hit.title.clone(),
            summary: hit.summary.clone(),
            importance: hit.importance,
            depth1_loaded: hit.depth1_loaded,
            kind: hit.kind.clone(),
            bm25_score: None,
            semantic_score: None,
        }
    }
}
