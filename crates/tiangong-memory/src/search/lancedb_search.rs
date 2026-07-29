//! 嵌入式 LanceDB 向量索引
//!
//! 使用 lancedb 在进程内提供向量检索，无需外部服务。

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::TryStreamExt;
use lancedb::arrow::arrow_array::{
    Array, FixedSizeListArray, Float32Array, RecordBatch, StringArray, builder::Float32Builder,
};
use lancedb::arrow::arrow_schema::{DataType, Field, Schema, SchemaRef};
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{DistanceType, Table, connect};

use crate::search::vector::VectorIndex;
use crate::types::{MemoryKind, RecallHit, VectorPoint};

const TABLE_NAME: &str = "memory_vectors";

pub(crate) struct LanceDbIndex {
    table: Table,
    dimension: usize,
    schema: SchemaRef,
}

impl LanceDbIndex {
    pub(crate) async fn open(base_dir: &Path, dimension: usize) -> Result<Self> {
        let lancedb_path = base_dir.join("lancedb");
        std::fs::create_dir_all(&lancedb_path)
            .with_context(|| format!("创建 LanceDB 目录失败: {}", lancedb_path.display()))?;

        // 自愈旧版 LanceDB 遗留的 namespace manifest：旧版会把 auto_cleanup
        // 配置写进 __manifest，与 lance 8.0.0 不兼容（触发 debug_assert，
        // 且 release 下可能令 namespace 刷新逻辑不一致）。检测到即删除整个
        // __manifest，由当前版本重建一个干净的；向量数据 memory_vectors.lance
        // 不受影响。失败不中断启动，交由上层降级为 BM25-only。
        if let Err(err) = heal_legacy_manifest(&lancedb_path) {
            tracing::warn!("Memory LanceDB __manifest 自愈失败，继续尝试打开: {err}");
        }

        let db = connect(lancedb_path.to_str().unwrap_or("."))
            .execute()
            .await
            .with_context(|| "连接 LanceDB 失败")?;

        let schema = build_schema(dimension);

        let table_names = db
            .table_names()
            .execute()
            .await
            .with_context(|| "获取 LanceDB 表列表失败")?;

        let table = if table_names.iter().any(|n| n == TABLE_NAME) {
            db.open_table(TABLE_NAME)
                .execute()
                .await
                .with_context(|| "打开 LanceDB 表失败")?
        } else {
            db.create_empty_table(TABLE_NAME, schema.clone())
                .execute()
                .await
                .with_context(|| "创建 LanceDB 表失败")?
        };

        Ok(Self {
            table,
            dimension,
            schema,
        })
    }
}

#[async_trait(?Send)]
impl VectorIndex for LanceDbIndex {
    async fn ensure_ready(&self) -> Result<()> {
        Ok(())
    }

    async fn upsert(&self, point: VectorPoint) -> Result<()> {
        if point.vector.len() != self.dimension {
            bail!(
                "LanceDB 向量维度不匹配: expected={} actual={}",
                self.dimension,
                point.vector.len()
            );
        }

        // LanceDB 没有原生 upsert，先删后增
        self.table
            .delete(&format!("node_id = '{}'", point.node_id))
            .await
            .ok(); // 忽略不存在的删除

        let batch = vector_point_to_batch(&point, &self.schema, self.dimension)?;
        self.table
            .add(batch)
            .execute()
            .await
            .with_context(|| "LanceDB 写入向量失败")?;

        Ok(())
    }

    async fn search(&self, query_vector: Vec<f32>, limit: usize) -> Result<Vec<RecallHit>> {
        if query_vector.len() != self.dimension {
            bail!(
                "LanceDB 查询向量维度不匹配: expected={} actual={}",
                self.dimension,
                query_vector.len()
            );
        }

        let batches = self
            .table
            .query()
            .nearest_to(query_vector)?
            .distance_type(DistanceType::Cosine)
            .limit(limit)
            .execute()
            .await
            .with_context(|| "LanceDB 向量搜索失败")?
            .try_collect::<Vec<_>>()
            .await
            .with_context(|| "LanceDB 搜索结果收集失败")?;

        Ok(record_batches_to_recall_hits(batches))
    }

    async fn delete(&self, node_id: &str) -> Result<()> {
        self.table
            .delete(&format!("node_id = '{node_id}'"))
            .await
            .with_context(|| format!("LanceDB 删除节点失败: {node_id}"))?;
        Ok(())
    }
}

fn build_schema(dimension: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("node_id", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("summary", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("importance", DataType::Float32, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dimension as i32,
            ),
            true,
        ),
    ]))
}

fn vector_point_to_batch(
    point: &VectorPoint,
    schema: &Schema,
    dimension: usize,
) -> Result<RecordBatch> {
    let mut vec_builder = Float32Builder::with_capacity(dimension);
    for &val in &point.vector {
        vec_builder.append_value(val);
    }
    let values = vec_builder.finish();
    let vector = FixedSizeListArray::new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        dimension as i32,
        Arc::new(values),
        None,
    );

    Ok(RecordBatch::try_new(
        Arc::new(schema.clone()),
        vec![
            Arc::new(StringArray::from(vec![point.node_id.clone()])),
            Arc::new(StringArray::from(vec![point.title.clone()])),
            Arc::new(StringArray::from(vec![point.summary.clone()])),
            Arc::new(StringArray::from(vec![format!("{:?}", point.kind)])),
            Arc::new(Float32Array::from(vec![point.importance as f32])),
            Arc::new(vector),
        ],
    )?)
}

fn record_batches_to_recall_hits(batches: Vec<RecordBatch>) -> Vec<RecallHit> {
    let mut hits = Vec::new();
    for batch in batches {
        let Some(node_ids) = batch
            .column_by_name("node_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        else {
            continue;
        };
        let titles = batch
            .column_by_name("title")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let summaries = batch
            .column_by_name("summary")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let kinds = batch
            .column_by_name("kind")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let importances = batch
            .column_by_name("importance")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
        let distances = batch
            .column_by_name("_distance")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

        for i in 0..node_ids.len() {
            let distance: f32 = distances.map(|d| d.value(i)).unwrap_or(0.0);
            let score = (1.0 - distance / 2.0).max(0.0_f32) as f64;
            hits.push(RecallHit {
                node_id: node_ids.value(i).to_string(),
                title: titles.map(|t| t.value(i).to_string()).unwrap_or_default(),
                summary: summaries
                    .map(|s| s.value(i).to_string())
                    .unwrap_or_default(),
                score,
                kind: kinds
                    .map(|k| parse_kind(k.value(i)))
                    .unwrap_or(MemoryKind::Episode),
                importance: importances.map(|imp| imp.value(i) as f64).unwrap_or(0.5),
                depth1_loaded: false,
            });
        }
    }
    hits
}

fn parse_kind(s: &str) -> MemoryKind {
    match s.to_lowercase().as_str() {
        "entity" => MemoryKind::Entity,
        "decision" => MemoryKind::Decision,
        "evidence" => MemoryKind::Evidence,
        _ => MemoryKind::Episode,
    }
}

/// LanceDB namespace manifest 配置键前缀。
///
/// lance 8.0.0 要求 namespace 内部的 `__manifest` 管理数据集**不得**启用
/// old-version cleanup（见 `lance-namespace-impls` 的 `DatasetConsistencyWrapper`
/// 构造断言）。旧版 LanceDB 创建 `__manifest` 时会写入这些键，升级后即不兼容。
const LEGACY_AUTO_CLEANUP_MARKER: &[u8] = b"lance.auto_cleanup.";

/// 扫描并自愈旧版 LanceDB 遗留的 namespace manifest。
///
/// 检测到 `__manifest` 的任意版本文件包含 `lance.auto_cleanup.` 配置时，
/// 删除整个 `__manifest` 目录，交由当前版本重建。读取/判断/删除均为纯字节
/// 操作，不依赖 lance 内部结构，对未来格式变动鲁棒。
fn heal_legacy_manifest(lancedb_dir: &Path) -> Result<()> {
    let manifest_dir = lancedb_dir.join("__manifest");
    if !manifest_dir.is_dir() {
        return Ok(());
    }

    // manifest 版本文件位于 _versions/ 下；兼容个别布局直接放在 __manifest/ 根。
    let versions_dir = manifest_dir.join("_versions");
    let scan_dirs = [versions_dir.as_path(), manifest_dir.as_path()];

    let mut tainted = false;
    for dir in scan_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("manifest") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if bytes
                .windows(LEGACY_AUTO_CLEANUP_MARKER.len())
                .any(|w| w == LEGACY_AUTO_CLEANUP_MARKER)
            {
                tainted = true;
                break;
            }
        }
        if tainted {
            break;
        }
    }

    if tainted {
        tracing::warn!(
            "Memory 检测到旧版 LanceDB __manifest 含 auto_cleanup 配置（与 lance 8.0.0 不兼容），正在删除以重建"
        );
        std::fs::remove_dir_all(&manifest_dir)
            .with_context(|| format!("删除遗留 __manifest 失败: {}", manifest_dir.display()))?;
        tracing::info!("Memory LanceDB __manifest 已清理，将由当前版本重建");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// 在 `root` 下写入一个 `__manifest` 版本文件，返回其路径。
    fn write_manifest(root: &Path, name: &str, content: &[u8]) -> PathBuf {
        let dir = root.join("__manifest").join("_versions");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.manifest"));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn clean_manifest_is_not_touched() {
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(tmp.path(), "1", b"plain manifest without cleanup config");
        // 不含 auto_cleanup 标记时不应删除。
        heal_legacy_manifest(tmp.path()).unwrap();
        assert!(path.exists(), "干净的 manifest 不应被删除");
        assert!(tmp.path().join("__manifest").exists());
    }

    #[test]
    fn dirty_manifest_is_removed() {
        let tmp = TempDir::new().unwrap();
        let path = write_manifest(
            tmp.path(),
            "1",
            b"some bytes lance.auto_cleanup.older_than 14days trailing",
        );
        // 命中标记时应删除整个 __manifest。
        heal_legacy_manifest(tmp.path()).unwrap();
        assert!(!path.exists(), "受污染的 manifest 应被删除");
        assert!(
            !tmp.path().join("__manifest").exists(),
            "整个 __manifest 目录应被删除"
        );
    }

    #[test]
    fn no_manifest_is_noop() {
        let tmp = TempDir::new().unwrap();
        // 无 __manifest 时应正常返回、无副作用。
        heal_legacy_manifest(tmp.path()).unwrap();
        assert!(!tmp.path().join("__manifest").exists());
    }

    #[test]
    fn any_tainted_file_triggers_removal() {
        let tmp = TempDir::new().unwrap();
        let clean = write_manifest(tmp.path(), "1", b"clean content");
        // 第二个文件命中标记，即便第一个干净也应整体删除。
        write_manifest(tmp.path(), "2", b"xxx lance.auto_cleanup.interval 20 yyy");
        heal_legacy_manifest(tmp.path()).unwrap();
        assert!(!clean.exists());
        assert!(!tmp.path().join("__manifest").exists());
    }
}
