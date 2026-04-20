//! Memory 存储协调器（仅 MemoryActor 内部访问）
//!
//! Phase A：协调 SQLite 元数据库 + Injection 文件读写。
//! Phase B：扩展为 SQLite + Tantivy 双层协调。
//! Phase C：扩展为 SQLite + Tantivy + Vector 三层协调（recall 通过 RecallEngine）。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tiangong_llm::{EmbeddingProvider, OpenAiEmbeddingProvider, ProviderProtocol};

use crate::command::InjectionLevel;
use crate::db::MemoryDb;
use crate::injection;
use crate::options::{MemoryEmbeddingConfig, MemoryVectorMode};
use crate::recall::RecallEngine;
use crate::search::TantivyIndex;
use crate::search::qdrant_search::QdrantIndex;
use crate::search::vector::{EmbeddedFlatVectorIndex, VectorIndex};
use crate::types::{
    Episode, MemoryKind, MemoryNode, MemoryScopeType, MemoryStatus, RecallAnchors, RecallHit,
};

/// Memory 存储协调器
pub(crate) struct MemoryStore {
    db: MemoryDb,
    tantivy: TantivyIndex,
    recall_engine: RecallEngine,
    workspace_id: Option<String>,
}

impl MemoryStore {
    /// 打开存储（初始化 SQLite + Tantivy，无向量索引时降级为 BM25）
    pub(crate) fn open(workspace_id: Option<String>) -> Result<Self> {
        let base = memory_base_dir();
        let db = MemoryDb::open()?;
        let tantivy = TantivyIndex::open(&base)?;
        // RecallEngine 不持有 TantivyIndex，避免同一索引目录双 writer 锁冲突
        let recall_engine = RecallEngine::bm25_only();
        Ok(Self {
            db,
            tantivy,
            recall_engine,
            workspace_id,
        })
    }

    /// 启用向量双引擎。
    pub(crate) fn enable_vector_index(
        &mut self,
        vector_index: Box<dyn VectorIndex>,
        embedding: Arc<dyn EmbeddingProvider>,
    ) {
        self.recall_engine = RecallEngine::dual(vector_index, embedding);
    }

    /// 基于上层传入的 embedding 配置启用向量索引。
    ///
    /// 失败时仅记录 warning，Memory 自动降级为 BM25-only。
    pub(crate) async fn try_enable_vector(
        &mut self,
        embedding: Option<&MemoryEmbeddingConfig>,
        vector_mode: MemoryVectorMode,
    ) {
        let Some(embedding) = embedding else {
            tracing::debug!("Memory embedding 未配置，使用 BM25-only 召回");
            return;
        };

        if embedding.protocol != ProviderProtocol::OpenAiCompatible {
            tracing::warn!(
                "Memory embedding 仅支持 OpenAI 兼容协议，当前协议为 {}，跳过向量层",
                embedding.protocol.as_str()
            );
            return;
        }

        if embedding.dimension == 0 {
            tracing::warn!("Memory embedding dimension 为 0，跳过向量层");
            return;
        }

        let vector_mode = match vector_mode {
            MemoryVectorMode::Auto => MemoryVectorMode::Embedded,
            mode => mode,
        };
        if vector_mode == MemoryVectorMode::Disabled {
            tracing::info!("Memory 向量层已禁用，使用 BM25-only 召回");
            return;
        }

        let vector_index: Box<dyn VectorIndex> = match vector_mode {
            MemoryVectorMode::Embedded => {
                match EmbeddedFlatVectorIndex::open(embedding.dimension) {
                    Ok(index) => Box::new(index),
                    Err(err) => {
                        tracing::warn!("Memory 内置向量索引初始化失败，使用 BM25-only 召回: {err}");
                        return;
                    }
                }
            }
            MemoryVectorMode::ExternalQdrant => {
                match QdrantIndex::connect(embedding.dimension).await {
                    Ok(index) => Box::new(index),
                    Err(err) => {
                        tracing::warn!("Memory Qdrant 连接失败，使用 BM25-only 召回: {err}");
                        return;
                    }
                }
            }
            MemoryVectorMode::Auto | MemoryVectorMode::Disabled => unreachable!(),
        };

        if let Err(err) = vector_index.ensure_ready().await {
            tracing::warn!("Memory 向量索引初始化失败，使用 BM25-only 召回: {err}");
            return;
        }

        let backend = match vector_mode {
            MemoryVectorMode::Embedded => "embedded_flat",
            MemoryVectorMode::ExternalQdrant => "external_qdrant",
            MemoryVectorMode::Auto | MemoryVectorMode::Disabled => unreachable!(),
        };

        let embedding_provider = Arc::new(OpenAiEmbeddingProvider::new(
            &embedding.base_url,
            &embedding.api_key,
            &embedding.model,
            embedding.dimension,
        ));
        self.enable_vector_index(vector_index, embedding_provider);
        tracing::info!(
            "Memory 向量双引擎召回已启用: backend={} model={} dimension={} timeout_ms={}",
            backend,
            embedding.model,
            embedding.dimension,
            embedding.timeout_ms
        );
    }

    /// 显式启用外部 Qdrant，供后续兼容接口使用。
    #[allow(dead_code)]
    pub(crate) async fn try_enable_qdrant(&mut self, embedding: Option<&MemoryEmbeddingConfig>) {
        self.try_enable_vector(embedding, MemoryVectorMode::ExternalQdrant)
            .await;
    }

    /// 加载三级注入上下文
    pub(crate) fn load_injection(
        &self,
        session_id: &str,
        workspace_id: Option<&str>,
    ) -> Vec<String> {
        let wid = workspace_id.or(self.workspace_id.as_deref());
        injection::load_injection_context(session_id, wid)
    }

    /// 写入 Episode（SQLite + Tantivy），workspace_id 由调用方显式传入
    pub(crate) async fn write_episode(
        &mut self,
        episode: Episode,
        workspace_id: Option<&str>,
    ) -> Result<()> {
        let node = self.write_episode_metadata(episode, workspace_id)?;
        if let Err(err) = self.recall_engine.upsert_node(&node).await {
            tracing::warn!("Memory 向量写入失败（非致命）: {err}");
        }
        Ok(())
    }

    fn write_episode_metadata(
        &mut self,
        episode: Episode,
        workspace_id: Option<&str>,
    ) -> Result<MemoryNode> {
        // 优先使用调用方传入的 workspace_id，回退到 store 启动时的值
        let wid = workspace_id.or(self.workspace_id.as_deref());
        // 1. 写入 SQLite（携带 workspace_id，保证 scope_id 正确落库）
        let node = episode_to_node(&episode, wid);
        self.db.insert_episode(&episode, wid)?;

        // 2. 写入 Tantivy 索引
        let body_extra = episode.tool_calls.join(" ");
        if let Err(e) = self.tantivy.index_node(&node, &body_extra) {
            tracing::warn!("Tantivy 索引写入失败（非致命）: {}", e);
        }

        // Phase C：Qdrant upsert 在 actor 层触发（异步，通过 RunEmbedAndUpsert 命令）

        Ok(node)
    }

    /// 渐进式召回（自动选择单引擎或双引擎）
    ///
    /// 注意：此方法需要 async 运行时（在 tokio task 内调用）
    pub(crate) async fn recall_async(
        &self,
        anchors: &RecallAnchors,
        limit: usize,
    ) -> Vec<RecallHit> {
        // BM25 由 MemoryStore 统一执行，保证只有一个 IndexWriter
        let bm25_hits = self
            .tantivy
            .search(&anchors.query, limit * 2)
            .unwrap_or_default();
        self.recall_engine
            .recall(bm25_hits, &anchors.query, limit)
            .await
    }

    /// 查询最近 Episode 摘要（用于 MesoRumination 提炼关键词）
    pub(crate) fn recent_episode_summaries(&self, limit: usize) -> Vec<(String, Vec<String>)> {
        self.db.recent_episode_summaries(limit).unwrap_or_default()
    }

    /// 列出低活跃节点（用于 MetaRumination 归档）
    pub(crate) fn list_stale_nodes(
        &self,
        days_threshold: i64,
        importance_threshold: f64,
    ) -> Vec<(String, f64)> {
        self.db
            .list_stale_nodes(days_threshold, importance_threshold)
            .unwrap_or_default()
    }

    /// 归档节点（MetaRumination 使用）
    pub(crate) fn archive_node(&mut self, node_id: &str) {
        if let Err(e) = self
            .db
            .update_node_status(node_id, &crate::types::MemoryStatus::Archived)
        {
            tracing::warn!("归档节点 {} 失败: {}", node_id, e);
        }
        // 从 Tantivy 中删除
        if let Err(e) = self.tantivy.delete_node(node_id) {
            tracing::warn!("从 Tantivy 删除节点 {} 失败（非致命）: {}", node_id, e);
        }
    }

    /// 全文搜索（Tantivy BM25，同步版，供兼容使用）
    #[allow(dead_code)]
    pub(crate) fn search(&self, query: &str, limit: usize) -> Vec<RecallHit> {
        self.tantivy.search(query, limit).unwrap_or_default()
    }

    /// 更新注入文件
    pub(crate) fn update_injection(
        &self,
        level: InjectionLevel,
        target_id: &str,
        content: &str,
    ) -> Result<()> {
        injection::write_injection_file(level, target_id, content)
    }
}

/// 将 Episode 转换为 MemoryNode（用于 Tantivy 索引）
fn episode_to_node(ep: &Episode, workspace_id: Option<&str>) -> MemoryNode {
    let now = chrono::Local::now().naive_local().to_string();
    MemoryNode {
        id: ep.id.clone(),
        kind: MemoryKind::Episode,
        scope_type: MemoryScopeType::Workspace,
        scope_id: workspace_id.map(String::from),
        title: ep.title.clone(),
        summary: ep.summary.clone(),
        keywords: ep.keywords.clone(),
        importance: ep.importance,
        confidence: 1.0,
        status: MemoryStatus::Active,
        source: Some(ep.session_id.clone()),
        usage_count: 0,
        last_used_at: None,
        created_at: ep.created_at.clone(),
        updated_at: now,
    }
}

fn memory_base_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("memory")
}

fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}
