//! 向量索引抽象与内置 flat 检索实现。
//!
//! 默认使用 SQLite 持久化向量并进行本地余弦相似度扫描，不要求用户启动
//! 额外向量数据库进程。外部 Qdrant 通过同一个 `VectorIndex` trait 接入。

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::db::MemoryDb;
use crate::types::{RecallHit, VectorPoint};

#[async_trait(?Send)]
pub(crate) trait VectorIndex: Send {
    async fn ensure_ready(&self) -> Result<()>;
    async fn upsert(&self, point: VectorPoint) -> Result<()>;
    async fn search(&self, query_vector: Vec<f32>, limit: usize) -> Result<Vec<RecallHit>>;
    #[allow(dead_code)]
    async fn delete(&self, node_id: &str) -> Result<()>;
}

/// 内置 flat 向量索引。
///
/// 适合桌面单用户场景：零外部服务，数据量较小时可预测、易调试。
pub(crate) struct EmbeddedFlatVectorIndex {
    db: MemoryDb,
    dimension: usize,
}

impl EmbeddedFlatVectorIndex {
    pub(crate) fn open(dimension: usize) -> Result<Self> {
        if dimension == 0 {
            bail!("内置向量索引维度不能为 0");
        }
        Ok(Self {
            db: MemoryDb::open()?,
            dimension,
        })
    }
}

#[async_trait(?Send)]
impl VectorIndex for EmbeddedFlatVectorIndex {
    async fn ensure_ready(&self) -> Result<()> {
        Ok(())
    }

    async fn upsert(&self, point: VectorPoint) -> Result<()> {
        if point.vector.len() != self.dimension {
            bail!(
                "内置向量维度不匹配: expected={} actual={}",
                self.dimension,
                point.vector.len()
            );
        }
        self.db.upsert_vector(&point)
    }

    async fn search(&self, query_vector: Vec<f32>, limit: usize) -> Result<Vec<RecallHit>> {
        if query_vector.len() != self.dimension {
            bail!(
                "查询向量维度不匹配: expected={} actual={}",
                self.dimension,
                query_vector.len()
            );
        }
        let mut hits: Vec<RecallHit> = self
            .db
            .list_vectors(self.dimension)?
            .into_iter()
            .filter_map(|point| {
                let score = cosine_similarity(&query_vector, &point.vector)?;
                Some(RecallHit {
                    node_id: point.node_id,
                    title: point.title,
                    summary: point.summary,
                    score,
                    kind: point.kind,
                    importance: point.importance,
                    depth1_loaded: false,
                })
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }

    async fn delete(&self, node_id: &str) -> Result<()> {
        self.db.delete_vector(node_id)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }

    let mut dot = 0.0_f64;
    let mut norm_a = 0.0_f64;
    let mut norm_b = 0.0_f64;
    for (left, right) in a.iter().zip(b.iter()) {
        let left = f64::from(*left);
        let right = f64::from(*right);
        dot += left * right;
        norm_a += left * left;
        norm_b += right * right;
    }

    if norm_a <= f64::EPSILON || norm_b <= f64::EPSILON {
        return None;
    }
    Some(dot / (norm_a.sqrt() * norm_b.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::cosine_similarity;

    #[test]
    fn cosine_similarity_orders_related_vectors() {
        let close = cosine_similarity(&[1.0, 0.0, 0.0], &[0.9, 0.1, 0.0]).unwrap();
        let far = cosine_similarity(&[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0]).unwrap();
        assert!(close > far);
    }
}
