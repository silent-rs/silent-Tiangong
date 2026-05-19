//! 嵌入式 Qdrant Edge 向量索引
//!
//! 使用 qdrant-edge 在进程内提供 HNSW 向量检索，无需外部服务。

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use qdrant_edge::{
    Distance, EdgeConfig, EdgeVectorParams, Payload, PointId, PointInsertOperations,
    PointOperations, PointStruct, QueryEnum, SearchRequest, UpdateOperation, Vectors,
    WithPayloadInterface,
};

use crate::search::vector::VectorIndex;
use crate::types::{MemoryKind, RecallHit, VectorPoint};

pub(crate) struct QdrantEdgeIndex {
    shard: qdrant_edge::EdgeShard,
    dimension: usize,
}

impl QdrantEdgeIndex {
    pub(crate) fn open(base_dir: &Path, dimension: usize) -> Result<Self> {
        let edge_path = base_dir.join("qdrant_edge");
        std::fs::create_dir_all(&edge_path)
            .with_context(|| format!("创建 Qdrant Edge 目录失败: {}", edge_path.display()))?;

        let config = EdgeConfig {
            vectors: HashMap::from([(
                String::new(),
                EdgeVectorParams {
                    size: dimension,
                    distance: Distance::Cosine,
                    on_disk: None,
                    multivector_config: None,
                    datatype: None,
                    quantization_config: None,
                    hnsw_config: None,
                },
            )]),
            ..EdgeConfig::default()
        };

        let has_segments = edge_path.join("segments").exists()
            && std::fs::read_dir(edge_path.join("segments"))
                .map(|mut d| d.next().is_some())
                .unwrap_or(false);

        let shard = if has_segments {
            qdrant_edge::EdgeShard::load(&edge_path, Some(config))
                .with_context(|| "加载 Qdrant Edge shard 失败")?
        } else {
            qdrant_edge::EdgeShard::new(&edge_path, config)
                .with_context(|| "创建 Qdrant Edge shard 失败")?
        };

        Ok(Self { shard, dimension })
    }
}

#[async_trait(?Send)]
impl VectorIndex for QdrantEdgeIndex {
    async fn ensure_ready(&self) -> Result<()> {
        Ok(())
    }

    async fn upsert(&self, point: VectorPoint) -> Result<()> {
        if point.vector.len() != self.dimension {
            bail!(
                "Qdrant Edge 向量维度不匹配: expected={} actual={}",
                self.dimension,
                point.vector.len()
            );
        }

        let payload = serde_json::json!({
            "node_id": point.node_id,
            "title": point.title,
            "summary": point.summary,
            "importance": point.importance,
            "kind": format!("{:?}", point.kind),
        });

        let point_id = hash_id_to_u64(&point.node_id);
        let vectors: Vectors = point.vector.into();
        let qdrant_point = PointStruct::new(PointId::NumId(point_id), vectors, payload);

        let operation = UpdateOperation::PointOperation(PointOperations::UpsertPoints(
            PointInsertOperations::PointsList(vec![qdrant_point.into()]),
        ));

        self.shard
            .update(operation)
            .with_context(|| "Qdrant Edge upsert 失败")?;

        Ok(())
    }

    async fn search(&self, query_vector: Vec<f32>, limit: usize) -> Result<Vec<RecallHit>> {
        if query_vector.len() != self.dimension {
            bail!(
                "查询向量维度不匹配: expected={} actual={}",
                self.dimension,
                query_vector.len()
            );
        }

        let request = SearchRequest {
            query: QueryEnum::from(query_vector),
            filter: None,
            params: None,
            limit,
            offset: 0,
            with_payload: Some(WithPayloadInterface::Bool(true)),
            with_vector: None,
            score_threshold: None,
        };

        let scored_points = self
            .shard
            .search(request)
            .with_context(|| "Qdrant Edge 搜索失败")?;

        let hits = scored_points
            .into_iter()
            .filter_map(|point| {
                let payload = point.payload?;
                let node_id = payload_str(&payload, "node_id")?;
                let title = payload_str(&payload, "title")
                    .unwrap_or_default()
                    .to_string();
                let summary = payload_str(&payload, "summary")
                    .unwrap_or_default()
                    .to_string();
                let importance = payload_f64(&payload, "importance").unwrap_or(0.5);
                let kind_str = payload_str(&payload, "kind").unwrap_or_default();

                Some(RecallHit {
                    node_id: node_id.to_string(),
                    title,
                    summary,
                    score: point.score as f64,
                    kind: parse_kind(kind_str),
                    importance,
                    depth1_loaded: false,
                })
            })
            .collect();

        Ok(hits)
    }

    async fn delete(&self, node_id: &str) -> Result<()> {
        let point_id = hash_id_to_u64(node_id);
        let operation = UpdateOperation::PointOperation(PointOperations::DeletePoints {
            ids: vec![PointId::NumId(point_id)],
        });
        self.shard
            .update(operation)
            .with_context(|| format!("Qdrant Edge 删除节点失败: {node_id}"))?;
        Ok(())
    }
}

fn payload_str<'a>(payload: &'a Payload, key: &str) -> Option<&'a str> {
    payload.0.get(key).and_then(|v| v.as_str())
}

fn payload_f64(payload: &Payload, key: &str) -> Option<f64> {
    payload.0.get(key).and_then(|v| v.as_str()?.parse().ok())
}

fn parse_kind(s: &str) -> MemoryKind {
    match s.to_lowercase().as_str() {
        "entity" => MemoryKind::Entity,
        "decision" => MemoryKind::Decision,
        "evidence" => MemoryKind::Evidence,
        _ => MemoryKind::Episode,
    }
}

fn hash_id_to_u64(id: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}
