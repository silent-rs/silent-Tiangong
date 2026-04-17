//! 召回结果融合与重排（Phase C）
//!
//! 将 Tantivy BM25 召回和 Qdrant 语义召回的结果融合、归一化、重排。

use crate::types::{MemoryKind, RecallHit};

/// 融合重排器
pub(crate) struct Reranker {
    /// BM25 召回权重（Keyword intent=0.7, Semantic=0.3, Hybrid=0.5）
    bm25_weight: f64,
    /// 语义召回权重（Keyword=0.3, Semantic=0.7, Hybrid=0.5）
    semantic_weight: f64,
}

impl Reranker {
    pub(crate) fn new(bm25_weight: f64, semantic_weight: f64) -> Self {
        Self {
            bm25_weight,
            semantic_weight,
        }
    }

    /// 根据 QueryIntent 创建 Reranker
    pub(crate) fn from_intent(intent: QueryIntent) -> Self {
        match intent {
            QueryIntent::Keyword => Self::new(0.7, 0.3),
            QueryIntent::Semantic => Self::new(0.3, 0.7),
            QueryIntent::Hybrid => Self::new(0.5, 0.5),
        }
    }

    /// 融合两路召回结果并返回 topK
    ///
    /// - 分数归一化：min-max
    /// - 双命中奖励：同时出现在两路结果中加 0.2
    /// - 重要度加权
    /// - 时间衰减（未来 Phase D 完善）
    pub(crate) fn fuse(
        &self,
        bm25: Vec<RecallHit>,
        semantic: Vec<RecallHit>,
        limit: usize,
    ) -> Vec<RecallHit> {
        use std::collections::HashMap;

        // 归一化 BM25 分数
        let bm25_norm = normalize_scores(bm25);
        // 归一化语义分数
        let semantic_norm = normalize_scores(semantic);

        // 合并：以 node_id 为 key，累加加权分数
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

        // 计算最终分数
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
                // 重要度加权（±10%）
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

/// Query 意图分析
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueryIntent {
    /// 偏关键词（含代码符号/文件路径/错误码）
    Keyword,
    /// 偏语义（自然语言、模糊引用）
    Semantic,
    /// 混合
    Hybrid,
}

/// 分析查询意图
pub(crate) fn analyze_query(text: &str) -> QueryIntent {
    let has_code_symbols = text.contains("::")
        || text.contains("->")
        || text.contains("fn ")
        || text.contains("impl ")
        || text.contains("struct ");
    let has_paths = text.contains('/')
        || text.contains('\\')
        || text.contains(".rs")
        || text.contains(".py")
        || text.contains(".ts");
    let has_error_codes = text.contains("error[") || text.contains("E0") || text.contains("panic");
    let has_vague_ref = text.contains("之前")
        || text.contains("那个")
        || text.contains("上次")
        || text.contains("刚才")
        || text.contains("last time")
        || text.contains("earlier");

    let precise = (has_code_symbols as u8) + (has_paths as u8) + (has_error_codes as u8);
    let vague = has_vague_ref as u8;

    match (precise > 0, vague > 0) {
        (true, false) => QueryIntent::Keyword,
        (false, true) => QueryIntent::Semantic,
        _ => QueryIntent::Hybrid,
    }
}

// ==================== 内部辅助 ====================

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

/// Min-Max 归一化，将分数归一化到 [0, 1]
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
