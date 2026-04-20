//! Qdrant 向量检索（Phase C）
//!
//! 提供对 Qdrant 的 collection 管理、point upsert 和语义查询。
//! 配置通过环境变量 QDRANT_URL（默认 http://127.0.0.1:6334）提供。

use anyhow::{Context, Result};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, PointStruct, SearchPointsBuilder, UpsertPointsBuilder,
    VectorParamsBuilder,
};

use crate::types::{MemoryKind, MemoryNode, RecallHit};

const DEFAULT_COLLECTION_NAME: &str = "tiangong_memory";
const PAYLOAD_FIELD_ID: &str = "node_id";
const PAYLOAD_FIELD_TITLE: &str = "title";
const PAYLOAD_FIELD_SUMMARY: &str = "summary";
const PAYLOAD_FIELD_IMPORTANCE: &str = "importance";
const PAYLOAD_FIELD_KIND: &str = "kind";
#[allow(dead_code)]
const PAYLOAD_FIELD_CREATED_AT: &str = "created_at";

/// Qdrant 向量搜索引擎
#[allow(dead_code)]
pub(crate) struct QdrantIndex {
    client: Qdrant,
    dimension: usize,
    collection_name: String,
}

impl QdrantIndex {
    /// 连接 Qdrant（从环境变量 QDRANT_URL 读取地址，默认 http://127.0.0.1:6334）
    #[allow(dead_code)]
    pub(crate) async fn connect(dimension: usize) -> Result<Self> {
        let url =
            std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6334".to_string());
        let client = Qdrant::from_url(&url)
            .build()
            .with_context(|| format!("连接 Qdrant 失败: {url}"))?;

        let collection_name = std::env::var("TIANGONG_MEMORY_QDRANT_COLLECTION")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_COLLECTION_NAME.to_string());

        Ok(Self {
            client,
            dimension,
            collection_name,
        })
    }

    /// 确保 collection 已创建（幂等）
    #[allow(dead_code)]
    pub(crate) async fn ensure_collection(&self) -> Result<()> {
        let collections = self
            .client
            .list_collections()
            .await
            .context("列出 Qdrant collections 失败")?;

        let exists = collections
            .collections
            .iter()
            .any(|c| c.name == self.collection_name);

        if !exists {
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(&self.collection_name).vectors_config(
                        VectorParamsBuilder::new(self.dimension as u64, Distance::Cosine),
                    ),
                )
                .await
                .with_context(|| {
                    format!("创建 Qdrant collection '{}' 失败", self.collection_name)
                })?;
            tracing::info!(
                "Qdrant collection '{}' 已创建（dimension={}）",
                self.collection_name,
                self.dimension
            );
        }
        Ok(())
    }

    /// 将 MemoryNode 的向量 upsert 到 Qdrant
    #[allow(dead_code)]
    pub(crate) async fn upsert_node(&self, node: &MemoryNode, vector: Vec<f32>) -> Result<()> {
        use qdrant_client::qdrant::value::Kind;
        use qdrant_client::qdrant::{Value, Vectors};
        use std::collections::HashMap;

        let mut payload: HashMap<String, Value> = HashMap::new();
        payload.insert(
            PAYLOAD_FIELD_ID.to_string(),
            Value {
                kind: Some(Kind::StringValue(node.id.clone())),
            },
        );
        payload.insert(
            PAYLOAD_FIELD_TITLE.to_string(),
            Value {
                kind: Some(Kind::StringValue(node.title.clone())),
            },
        );
        payload.insert(
            PAYLOAD_FIELD_SUMMARY.to_string(),
            Value {
                kind: Some(Kind::StringValue(node.summary.clone())),
            },
        );
        payload.insert(
            PAYLOAD_FIELD_IMPORTANCE.to_string(),
            Value {
                kind: Some(Kind::DoubleValue(node.importance as f64)),
            },
        );
        payload.insert(
            PAYLOAD_FIELD_KIND.to_string(),
            Value {
                kind: Some(Kind::StringValue(format!("{:?}", node.kind))),
            },
        );
        payload.insert(
            PAYLOAD_FIELD_CREATED_AT.to_string(),
            Value {
                kind: Some(Kind::StringValue(node.created_at.clone())),
            },
        );

        let point_id = hash_id_to_u64(&node.id);
        let point = PointStruct::new(point_id, Vectors::from(vector), payload);

        self.client
            .upsert_points(UpsertPointsBuilder::new(&self.collection_name, vec![point]))
            .await
            .context("Qdrant upsert 失败")?;

        Ok(())
    }

    /// 语义向量搜索
    #[allow(dead_code)]
    pub(crate) async fn search(
        &self,
        query_vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<RecallHit>> {
        use qdrant_client::qdrant::value::Kind;

        let results = self
            .client
            .search_points(
                SearchPointsBuilder::new(&self.collection_name, query_vector, limit as u64)
                    .with_payload(true),
            )
            .await
            .context("Qdrant 语义搜索失败")?;

        let hits = results
            .result
            .into_iter()
            .filter_map(|scored| {
                let score = scored.score as f64;
                let payload = &scored.payload;

                let id = extract_str(payload, PAYLOAD_FIELD_ID)?;
                let title = extract_str(payload, PAYLOAD_FIELD_TITLE).unwrap_or_default();
                let summary = extract_str(payload, PAYLOAD_FIELD_SUMMARY).unwrap_or_default();
                let importance = payload
                    .get(PAYLOAD_FIELD_IMPORTANCE)
                    .and_then(|v| match v.kind.as_ref()? {
                        Kind::DoubleValue(f) => Some(*f),
                        _ => None,
                    })
                    .unwrap_or(0.5);
                let kind_str = extract_str(payload, PAYLOAD_FIELD_KIND).unwrap_or_default();

                Some(RecallHit {
                    node_id: id,
                    title,
                    summary,
                    score,
                    importance,
                    kind: parse_kind(&kind_str),
                    depth1_loaded: false,
                })
            })
            .collect();

        Ok(hits)
    }
}

fn extract_str(
    payload: &std::collections::HashMap<String, qdrant_client::qdrant::Value>,
    key: &str,
) -> Option<String> {
    use qdrant_client::qdrant::value::Kind;
    match payload.get(key)?.kind.as_ref()? {
        Kind::StringValue(s) => Some(s.clone()),
        _ => None,
    }
}

fn parse_kind(s: &str) -> MemoryKind {
    match s.to_lowercase().as_str() {
        "entity" => MemoryKind::Entity,
        "decision" => MemoryKind::Decision,
        "evidence" => MemoryKind::Evidence,
        _ => MemoryKind::Episode,
    }
}

/// 将字符串 id hash 为 u64（Qdrant point id）
#[allow(dead_code)]
fn hash_id_to_u64(id: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}
