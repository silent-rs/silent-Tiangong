//! 渐进式召回（Phase C）
//!
//! 协调 Tantivy BM25 召回和 Qdrant 语义召回，融合重排后返回最终结果。

use std::sync::Arc;

use tiangong_llm::EmbeddingProvider;

use crate::search::qdrant_search::QdrantIndex;
use crate::search::reranker::{Reranker, analyze_query};
use crate::search::tantivy_search::TantivyIndex;
use crate::types::{RecallAnchors, RecallHit};

/// 召回引擎（可选 Qdrant，降级为纯 BM25）
pub(crate) struct RecallEngine {
    tantivy: TantivyIndex,
    qdrant: Option<QdrantIndex>,
    embedding: Option<Arc<dyn EmbeddingProvider>>,
}

impl RecallEngine {
    /// 纯 BM25 模式（Phase B 兼容）
    pub(crate) fn bm25_only(tantivy: TantivyIndex) -> Self {
        Self {
            tantivy,
            qdrant: None,
            embedding: None,
        }
    }

    /// 双引擎模式（Tantivy + Qdrant）
    pub(crate) fn dual(
        tantivy: TantivyIndex,
        qdrant: QdrantIndex,
        embedding: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self {
            tantivy,
            qdrant: Some(qdrant),
            embedding: Some(embedding),
        }
    }

    /// 执行召回（自动选择单引擎或双引擎）
    pub(crate) async fn recall(&self, anchors: &RecallAnchors, limit: usize) -> Vec<RecallHit> {
        let intent = analyze_query(&anchors.query);
        let reranker = Reranker::from_intent(intent);

        // Tantivy BM25 召回
        let bm25_hits = self
            .tantivy
            .search(&anchors.query, limit * 2)
            .unwrap_or_default();

        // 若没有 Qdrant，退化为纯 BM25
        let (qdrant_ref, emb_ref) = match (self.qdrant.as_ref(), self.embedding.as_ref()) {
            (Some(q), Some(e)) => (q, e),
            _ => {
                return bm25_hits.into_iter().take(limit).collect();
            }
        };

        // 向量化 query
        let vectors = match emb_ref.embed(vec![anchors.query.clone()]).await {
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
