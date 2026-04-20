//! 渐进式召回（Phase C）
//!
//! 协调 Tantivy BM25 召回（由 MemoryStore 提供）和 Qdrant 语义召回，融合重排后返回最终结果。
//! RecallEngine 不持有 TantivyIndex，避免同一索引目录双 writer 锁冲突。

use std::sync::Arc;

use tiangong_llm::EmbeddingProvider;

use crate::search::embedding::node_embedding_text;
use crate::search::qdrant_search::QdrantIndex;
use crate::search::reranker::{Reranker, analyze_query};
use crate::types::{MemoryNode, RecallHit};

/// 召回引擎（可选 Qdrant，降级为纯 BM25）
///
/// BM25 搜索由外部（MemoryStore）执行后作为 `bm25_hits` 传入；
/// RecallEngine 只负责可选的 Qdrant 增强与最终融合重排。
pub(crate) struct RecallEngine {
    qdrant: Option<QdrantIndex>,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
}

impl RecallEngine {
    /// 纯 BM25 模式（Phase B 兼容）
    pub(crate) fn bm25_only() -> Self {
        Self {
            qdrant: None,
            embedding: None,
        }
    }

    /// 双引擎模式（Tantivy + Qdrant）
    pub(crate) fn dual(qdrant: QdrantIndex, embedding: Arc<dyn EmbeddingProvider>) -> Self {
        Self {
            qdrant: Some(qdrant),
            embedding: Some(embedding),
        }
    }

    /// 将节点写入可选语义索引。未启用 Qdrant 时直接跳过。
    pub(crate) async fn upsert_node(&self, node: &MemoryNode) -> anyhow::Result<()> {
        let (qdrant_ref, emb_ref) = match (self.qdrant.as_ref(), self.embedding.as_ref()) {
            (Some(q), Some(e)) => (q, e),
            _ => return Ok(()),
        };

        let text = node_embedding_text(node);
        let mut vectors = emb_ref.embed(vec![text]).await?;
        let Some(vector) = vectors.pop() else {
            anyhow::bail!("Memory node embedding 返回空结果");
        };
        qdrant_ref.upsert_node(node, vector).await
    }

    /// 执行召回：接受已完成的 BM25 结果，可选地用 Qdrant 增强后融合重排
    pub(crate) async fn recall(
        &self,
        bm25_hits: Vec<RecallHit>,
        query: &str,
        limit: usize,
    ) -> Vec<RecallHit> {
        let intent = analyze_query(query);
        let reranker = Reranker::from_intent(intent);

        // 若没有 Qdrant，退化为纯 BM25
        let (qdrant_ref, emb_ref) = match (self.qdrant.as_ref(), self.embedding.as_ref()) {
            (Some(q), Some(e)) => (q, e),
            _ => {
                return bm25_hits.into_iter().take(limit).collect();
            }
        };

        // 向量化 query
        let vectors = match emb_ref.embed(vec![query.to_string()]).await {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => return bm25_hits.into_iter().take(limit).collect(),
            Err(e) => {
                tracing::warn!("Embedding 失败，退化为 BM25 召回: {}", e);
                return bm25_hits.into_iter().take(limit).collect();
            }
        };

        // Qdrant 语义召回
        let semantic_hits = match qdrant_ref.search(vectors[0].clone(), limit * 2).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("Qdrant 搜索失败，退化为 BM25 召回: {}", e);
                return bm25_hits.into_iter().take(limit).collect();
            }
        };

        // 融合重排
        reranker.fuse(bm25_hits, semantic_hits, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryKind;

    fn make_hits(count: usize) -> Vec<RecallHit> {
        (0..count)
            .map(|i| RecallHit {
                node_id: format!("node-{i}"),
                title: format!("标题 {i}"),
                summary: format!("摘要内容 {i}"),
                score: 1.0 - (i as f64 * 0.05),
                kind: MemoryKind::Episode,
                importance: 0.5,
                depth1_loaded: false,
            })
            .collect()
    }

    #[tokio::test]
    async fn bm25_only_limits_results_to_requested_count() {
        let engine = RecallEngine::bm25_only();
        let hits = make_hits(10);
        let result = engine.recall(hits, "测试查询", 5).await;
        assert_eq!(result.len(), 5);
    }

    #[tokio::test]
    async fn bm25_only_returns_all_when_limit_exceeds_hits() {
        let engine = RecallEngine::bm25_only();
        let hits = make_hits(3);
        let result = engine.recall(hits, "测试查询", 10).await;
        assert_eq!(result.len(), 3, "结果数不应超过实际命中数");
    }

    #[tokio::test]
    async fn bm25_only_returns_empty_for_empty_input() {
        let engine = RecallEngine::bm25_only();
        let result = engine.recall(vec![], "测试查询", 5).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn bm25_only_preserves_order_by_score() {
        let engine = RecallEngine::bm25_only();
        let hits = make_hits(5); // score 从高到低
        let result = engine.recall(hits, "测试查询", 5).await;
        // 保持输入顺序（无 Qdrant 时不重排）
        for (i, hit) in result.iter().enumerate() {
            assert_eq!(hit.node_id, format!("node-{i}"));
        }
    }
}
