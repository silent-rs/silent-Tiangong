//! Memory 存储协调器（仅 MemoryActor 内部访问）
//!
//! Phase A：协调 SQLite 元数据库 + Injection 文件读写。
//! Phase B：扩展为 SQLite + Tantivy 双层协调。
//! Phase C：扩展为 SQLite + Tantivy + Vector 三层协调（recall 通过 RecallEngine）。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tiangong_llm::{
    EmbeddingEndpointConfig, EmbeddingProvider, RerankEndpointConfig, RerankProvider,
    embedding_provider_from_config, rerank_provider_from_config,
};

use crate::command::InjectionLevel;
use crate::db::MemoryDb;
use crate::db::migration;
use crate::injection;
use crate::options::MemoryVectorMode;
use crate::recall::RecallEngine;
use crate::search::TantivyIndex;
use crate::search::lancedb_search::LanceDbIndex;
use crate::search::qdrant_search::QdrantIndex;
use crate::search::vector::VectorIndex;
use crate::types::{
    Decision, Entity, Episode, EpisodeOutcome, ExpandedMemory, ManualMemoryDraft,
    MemoryCognitiveType, MemoryKind, MemoryListQuery, MemoryNode, MemoryRelation,
    MemoryRelationDraft, MemoryRelationKind, MemoryScopeType, MemoryStatus, RecallAnchors,
    RecallHit,
};
/// Memory 存储协调器
pub(crate) struct MemoryStore {
    db: MemoryDb,
    /// Tantivy 全文索引，多实例锁冲突时降级为 None（纯 SQLite 模式）
    tantivy: Option<TantivyIndex>,
    recall_engine: RecallEngine,
    workspace_id: Option<String>,
}

impl MemoryStore {
    /// 打开存储（初始化 SQLite + Tantivy，Tantivy 锁冲突时降级为纯 SQLite 模式）
    pub(crate) fn open(workspace_id: Option<String>) -> Result<Self> {
        let base = memory_index_base_dir(workspace_id.as_deref());
        let db = MemoryDb::open()?;

        // Tantivy 初始化失败时优雅降级（多实例锁冲突等场景）
        let tantivy = match TantivyIndex::open(&base) {
            Ok(idx) => Some(idx),
            Err(e) => {
                tracing::warn!("Tantivy 全文索引初始化失败，降级为纯 SQLite 模式: {}", e);
                None
            }
        };

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
        rerank: Option<Arc<dyn RerankProvider>>,
    ) {
        self.recall_engine = RecallEngine::dual(vector_index, embedding, rerank);
    }

    pub(crate) fn enable_rerank_only(&mut self, rerank: Arc<dyn RerankProvider>) {
        self.recall_engine = RecallEngine::rerank_only(rerank);
    }

    /// 重新配置召回增强层。
    ///
    /// 热更新时先回到 BM25-only，再尝试按新配置启用向量层。这样 embedding
    /// 维度或后端变化不会继续复用旧向量索引；新配置不可用时也能明确降级。
    pub(crate) async fn reconfigure_recall_engine(
        &mut self,
        embedding: Option<&EmbeddingEndpointConfig>,
        rerank: Option<&RerankEndpointConfig>,
        vector_mode: MemoryVectorMode,
    ) {
        self.recall_engine = RecallEngine::bm25_only();
        self.try_enable_recall_engine(embedding, rerank, vector_mode)
            .await;
    }

    /// 基于上层传入的 embedding / rerank 配置启用召回增强层。
    ///
    /// 失败时仅记录 warning，Memory 自动降级为 BM25-only。
    pub(crate) async fn try_enable_recall_engine(
        &mut self,
        embedding: Option<&EmbeddingEndpointConfig>,
        rerank: Option<&RerankEndpointConfig>,
        vector_mode: MemoryVectorMode,
    ) {
        let rerank_provider = rerank.and_then(|rerank| match rerank_provider_from_config(rerank) {
            Ok(provider) => Some(provider),
            Err(err) => {
                tracing::warn!("Memory rerank provider 初始化失败，跳过模型精排: {err}");
                None
            }
        });

        let Some(embedding) = embedding else {
            if let Some(rerank_provider) = rerank_provider {
                let model = rerank_provider.model().to_string();
                self.enable_rerank_only(rerank_provider);
                tracing::info!(model = %model, "Memory rerank 已启用，使用 BM25 + 模型精排");
            } else {
                tracing::debug!("Memory embedding/rerank 未配置，使用 BM25-only 召回");
            }
            return;
        };

        if embedding.dimension == 0 {
            if let Some(rerank_provider) = rerank_provider {
                let model = rerank_provider.model().to_string();
                self.enable_rerank_only(rerank_provider);
                tracing::warn!(
                    model = %model,
                    "Memory embedding dimension 为 0，跳过向量层，仅启用 rerank"
                );
            } else {
                tracing::warn!("Memory embedding dimension 为 0，跳过向量层");
            }
            return;
        }

        let embedding_provider = match embedding_provider_from_config(embedding) {
            Ok(provider) => provider,
            Err(err) => {
                if let Some(rerank_provider) = rerank_provider {
                    let model = rerank_provider.model().to_string();
                    self.enable_rerank_only(rerank_provider);
                    tracing::warn!(
                        model = %model,
                        "Memory embedding provider 初始化失败，仅启用 rerank: {err}"
                    );
                } else {
                    tracing::warn!("Memory embedding provider 初始化失败，跳过向量层: {err}");
                }
                return;
            }
        };

        let vector_mode = match vector_mode {
            MemoryVectorMode::Auto => default_vector_mode(),
            mode => mode,
        };
        if vector_mode == MemoryVectorMode::Disabled {
            if let Some(rerank_provider) = rerank_provider {
                let model = rerank_provider.model().to_string();
                self.enable_rerank_only(rerank_provider);
                tracing::info!(model = %model, "Memory 向量层已禁用，使用 BM25 + 模型精排");
            } else {
                tracing::info!("Memory 向量层已禁用，使用 BM25-only 召回");
            }
            return;
        }

        let (vector_index, backend): (Box<dyn VectorIndex>, &str) = match vector_mode {
            MemoryVectorMode::EmbeddedLanceDb => {
                let base = memory_index_base_dir(self.workspace_id.as_deref());
                let needs_migration = migration::needs_vector_migration(&self.db, &base);
                migration::cleanup_legacy_qdrant_edge(&base);
                match LanceDbIndex::open(&base, embedding.dimension).await {
                    Ok(index) => {
                        if needs_migration {
                            migration::migrate_vectors(&self.db, &index, embedding.dimension).await;
                        }
                        (Box::new(index), "embedded_lancedb")
                    }
                    Err(err) => {
                        if let Some(rerank_provider) = rerank_provider {
                            let model = rerank_provider.model().to_string();
                            self.enable_rerank_only(rerank_provider);
                            tracing::warn!(
                                model = %model,
                                "Memory LanceDB 索引初始化失败，仅启用 rerank: {err}"
                            );
                        } else {
                            tracing::warn!(
                                "Memory LanceDB 索引初始化失败，使用 BM25-only 召回: {err}"
                            );
                        }
                        return;
                    }
                }
            }
            MemoryVectorMode::ExternalQdrant => {
                match QdrantIndex::connect(embedding.dimension).await {
                    Ok(index) => (Box::new(index), "external_qdrant"),
                    Err(err) => {
                        if let Some(rerank_provider) = rerank_provider {
                            let model = rerank_provider.model().to_string();
                            self.enable_rerank_only(rerank_provider);
                            tracing::warn!(
                                model = %model,
                                "Memory Qdrant 连接失败，仅启用 rerank: {err}"
                            );
                        } else {
                            tracing::warn!("Memory Qdrant 连接失败，使用 BM25-only 召回: {err}");
                        }
                        return;
                    }
                }
            }
            MemoryVectorMode::Auto | MemoryVectorMode::Disabled => unreachable!(),
        };

        if let Err(err) = vector_index.ensure_ready().await {
            if let Some(rerank_provider) = rerank_provider {
                let model = rerank_provider.model().to_string();
                self.enable_rerank_only(rerank_provider);
                tracing::warn!(model = %model, "Memory 向量索引初始化失败，仅启用 rerank: {err}");
            } else {
                tracing::warn!("Memory 向量索引初始化失败，使用 BM25-only 召回: {err}");
            }
            return;
        }

        let rerank_model = rerank_provider
            .as_ref()
            .map(|provider| provider.model().to_string());
        self.enable_vector_index(vector_index, embedding_provider, rerank_provider);
        tracing::info!(
            "Memory 向量双引擎召回已启用: backend={} embedding_model={} dimension={} timeout_ms={} rerank_model={}",
            backend,
            embedding.model,
            embedding.dimension,
            embedding.timeout.as_millis(),
            rerank_model.as_deref().unwrap_or("none")
        );
    }

    /// 显式启用外部 Qdrant，供后续兼容接口使用。
    #[allow(dead_code)]
    pub(crate) async fn try_enable_qdrant(&mut self, embedding: Option<&EmbeddingEndpointConfig>) {
        self.try_enable_recall_engine(embedding, None, MemoryVectorMode::ExternalQdrant)
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
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.commit()
        {
            tracing::warn!("Tantivy commit 失败（非致命）: {e}");
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
        tracing::debug!(
            node_id = %node.id,
            workspace_id = ?wid,
            title = %node.title,
            "Memory Episode 元数据写入完成"
        );

        // 2. 写入 Tantivy 索引（可选，降级时跳过）
        let body_extra = episode.tool_calls.join(" ");
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.index_node(&node, &body_extra)
        {
            tracing::warn!("Tantivy 索引写入失败（非致命）: {}", e);
        }

        // Phase C：Qdrant upsert 在 actor 层触发（异步，通过 RunEmbedAndUpsert 命令）

        Ok(node)
    }

    /// 更新已有 Episode 的摘要和关键词（去重时使用）。
    pub(crate) async fn update_episode_summary(
        &mut self,
        node_id: &str,
        summary: &str,
        keywords: &[String],
    ) -> Result<()> {
        self.db.update_episode_summary(node_id, summary, keywords)?;
        if let Some(ref mut tantivy) = self.tantivy
            && let Some(node) = self.db.load_node(node_id)?
            && let Err(e) = tantivy.index_node(&node, summary)
        {
            tracing::warn!("Tantivy Episode 更新索引失败（非致命）: {}", e);
        }
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.commit()
        {
            tracing::warn!("Tantivy commit 失败（非致命）: {e}");
        }
        Ok(())
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
            .as_ref()
            .and_then(|tantivy| tantivy.search(&anchors.query, limit * 2).ok())
            .unwrap_or_default();
        tracing::debug!(
            query = %anchors.query,
            strategy = ?anchors.strategy,
            bm25_hit_count = bm25_hits.len(),
            backend = "bm25",
            "Memory BM25 召回完成"
        );
        self.recall_engine
            .recall(bm25_hits, &anchors.query, limit, anchors.strategy.as_ref())
            .await
    }

    /// 加载已召回节点的完整内容，用于二跳展开。
    pub(crate) fn load_depth2(&self, node_ids: &[String]) -> Vec<ExpandedMemory> {
        if node_ids.is_empty() {
            return Vec::new();
        }
        match self.db.load_expanded_memories(node_ids) {
            Ok(items) => items,
            Err(err) => {
                tracing::warn!("Memory LoadDepth2 展开失败: {err}");
                Vec::new()
            }
        }
    }

    /// 列出记忆节点，供 GUI 手动管理使用。
    pub(crate) fn list_nodes(&self, query: &MemoryListQuery) -> Vec<MemoryNode> {
        let workspace_id = query
            .workspace_id
            .as_deref()
            .or(self.workspace_id.as_deref());
        self.db
            .list_memory_nodes(
                workspace_id,
                query.query.as_deref(),
                query.status.as_ref(),
                query.created_after.as_deref(),
                query.offset,
                query.limit,
            )
            .unwrap_or_default()
    }

    pub(crate) fn count_nodes(&self, query: &MemoryListQuery) -> usize {
        let workspace_id = query
            .workspace_id
            .as_deref()
            .or(self.workspace_id.as_deref());
        self.db
            .count_memory_nodes(
                workspace_id,
                query.query.as_deref(),
                query.status.as_ref(),
                query.created_after.as_deref(),
            )
            .unwrap_or_default()
    }

    /// 手动新增或调整一条 Episode 记忆。
    pub(crate) async fn upsert_manual_memory(
        &mut self,
        draft: ManualMemoryDraft,
    ) -> Result<MemoryNode> {
        let workspace_id = draft
            .workspace_id
            .as_deref()
            .or(self.workspace_id.as_deref())
            .map(str::to_string);
        let keywords = normalize_keywords(draft.keywords);
        let importance = if draft.importance > 0.0 {
            draft.importance.clamp(0.0, 1.0)
        } else {
            0.6
        };
        let session_id = draft
            .session_id
            .filter(|value| !value.trim().is_empty())
            .or_else(|| Some("manual".to_string()))
            .unwrap_or_else(|| "manual".to_string());

        let episode = Episode {
            id: draft.id.unwrap_or_else(|| scru128::new().to_string()),
            session_id,
            memory_type: draft.memory_type.clone(),
            title: draft.title.trim().to_string(),
            summary: draft.summary.trim().to_string(),
            outcome: EpisodeOutcome::Success,
            keywords,
            tool_calls: Vec::new(),
            importance,
            created_at: chrono::Local::now().naive_local().to_string(),
        };
        let mut node = self.write_episode_metadata(episode, workspace_id.as_deref())?;
        node = self.db.update_memory_node_details(
            &node.id,
            &node.title,
            &node.summary,
            &node.keywords,
            node.importance,
            &draft.memory_type,
        )?;
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(err) = tantivy.index_node(&node, "")
        {
            tracing::warn!("Tantivy 手动记忆类型索引更新失败（非致命）: {err}");
        }
        if let Err(err) = self.recall_engine.upsert_node(&node).await {
            tracing::warn!("Memory 手动记忆向量写入失败（非致命）: {err}");
        }
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.commit()
        {
            tracing::warn!("Tantivy commit 失败（非致命）: {e}");
        }
        Ok(node)
    }

    /// 手动调整已有节点元信息。
    pub(crate) async fn update_manual_memory(
        &mut self,
        draft: ManualMemoryDraft,
    ) -> Result<MemoryNode> {
        let Some(node_id) = draft.id.as_deref().filter(|value| !value.trim().is_empty()) else {
            return self.upsert_manual_memory(draft).await;
        };
        let keywords = normalize_keywords(draft.keywords);
        let importance = if draft.importance > 0.0 {
            draft.importance.clamp(0.0, 1.0)
        } else {
            0.6
        };
        let node = self.db.update_memory_node_details(
            node_id,
            draft.title.trim(),
            draft.summary.trim(),
            &keywords,
            importance,
            &draft.memory_type,
        )?;
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(err) = tantivy.index_node(&node, "")
        {
            tracing::warn!("Tantivy 手动记忆重建索引失败（非致命）: {err}");
        }
        if let Err(err) = self.recall_engine.upsert_node(&node).await {
            tracing::warn!("Memory 手动记忆向量更新失败（非致命）: {err}");
        }
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.commit()
        {
            tracing::warn!("Tantivy commit 失败（非致命）: {e}");
        }
        Ok(node)
    }

    /// 按节点 ID 加载 RecallHit 元数据，供 deep recall 关系追溯使用。
    pub(crate) fn load_hits_by_ids(&self, node_ids: &[String]) -> Vec<RecallHit> {
        if node_ids.is_empty() {
            return Vec::new();
        }
        match self.db.load_recall_hits_by_ids(node_ids) {
            Ok(items) => items,
            Err(err) => {
                tracing::warn!("Memory 关系追溯加载节点失败: {err}");
                Vec::new()
            }
        }
    }

    /// 新增或调整记忆图关系。
    pub(crate) fn upsert_relation(&self, draft: MemoryRelationDraft) -> Result<MemoryRelation> {
        self.db.upsert_memory_relation(draft)
    }

    /// 列出某个节点的入边和出边关系。
    pub(crate) fn list_relations(&self, node_id: &str) -> Vec<MemoryRelation> {
        self.db.list_memory_relations(node_id).unwrap_or_default()
    }

    /// 批量列出多个节点的关联关系（去重）。
    pub(crate) fn list_relations_batch(&self, node_ids: &[String]) -> Vec<MemoryRelation> {
        self.db
            .list_memory_relations_batch(node_ids)
            .unwrap_or_default()
    }

    /// 删除记忆图关系。
    pub(crate) fn delete_relation(&self, relation_id: &str) -> Result<()> {
        self.db.delete_memory_relation(relation_id)
    }

    /// 读取图邻接节点，供 deep recall 关系追溯使用。
    pub(crate) fn related_node_ids(&self, node_ids: &[String]) -> Vec<String> {
        self.db.list_related_node_ids(node_ids).unwrap_or_default()
    }

    /// 为新写入的 Episode 自动关联近期相似记忆，避免图谱只出现孤立节点。
    pub(crate) fn link_episode_to_related_recent(
        &self,
        episode: &Episode,
        workspace_id: Option<&str>,
    ) -> Result<usize> {
        let recent = self.recent_episodes(workspace_id, 20);
        let mut linked = 0;
        for candidate in recent.iter().filter(|item| item.id != episode.id) {
            let Some((weight, note)) = episode_relation_signal(episode, candidate) else {
                continue;
            };
            self.upsert_relation(MemoryRelationDraft {
                id: None,
                from_node_id: episode.id.clone(),
                to_node_id: candidate.id.clone(),
                relation_kind: MemoryRelationKind::RelatedTo,
                weight,
                note: Some(note),
            })?;
            linked += 1;
            if linked >= 5 {
                break;
            }
        }
        Ok(linked)
    }

    /// 查询最近 Episode 摘要（用于 MesoRumination 提炼关键词）
    #[allow(dead_code)]
    pub(crate) fn recent_episode_summaries(&self, limit: usize) -> Vec<(String, Vec<String>)> {
        self.db.recent_episode_summaries(limit).unwrap_or_default()
    }

    /// 查询当前工作区最近 Episode 的完整内容，供 MesoRumination 提炼结构化记忆。
    pub(crate) fn recent_episodes(&self, workspace_id: Option<&str>, limit: usize) -> Vec<Episode> {
        self.db
            .recent_episodes(workspace_id, limit)
            .unwrap_or_default()
    }

    /// 查询当前工作区内指定会话最近 Episode 的完整内容。
    pub(crate) fn recent_episodes_for_session(
        &self,
        workspace_id: Option<&str>,
        session_id: &str,
        limit: usize,
    ) -> Vec<Episode> {
        self.db
            .recent_episodes_for_session(workspace_id, session_id, limit)
            .unwrap_or_default()
    }

    /// 写入 Entity（SQLite + Tantivy）。
    pub(crate) fn upsert_entity(
        &mut self,
        entity: Entity,
        workspace_id: Option<&str>,
    ) -> Result<()> {
        let node = entity_to_node(&entity, workspace_id);
        self.db.upsert_entity(&entity, workspace_id)?;
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.index_node(&node, &entity.name)
        {
            tracing::warn!("Tantivy Entity 索引写入失败（非致命）: {}", e);
        }
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.commit()
        {
            tracing::warn!("Tantivy commit 失败（非致命）: {e}");
        }
        Ok(())
    }

    /// 写入 Decision（SQLite + Tantivy）。
    pub(crate) fn upsert_decision(
        &mut self,
        decision: Decision,
        workspace_id: Option<&str>,
    ) -> Result<()> {
        let node = decision_to_node(&decision, workspace_id);
        self.db.upsert_decision(&decision, workspace_id)?;
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.index_node(&node, &decision.chosen)
        {
            tracing::warn!("Tantivy Decision 索引写入失败（非致命）: {}", e);
        }
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.commit()
        {
            tracing::warn!("Tantivy commit 失败（非致命）: {e}");
        }
        Ok(())
    }

    /// 列出 Entity，供 MesoRumination 幂等更新使用。
    pub(crate) fn list_entities(&self, workspace_id: Option<&str>) -> Vec<Entity> {
        self.db.list_entities(workspace_id).unwrap_or_default()
    }

    /// 列出 Decision，供 MesoRumination 幂等更新使用。
    pub(crate) fn list_decisions(&self, workspace_id: Option<&str>) -> Vec<Decision> {
        self.db.list_decisions(workspace_id).unwrap_or_default()
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
    pub(crate) async fn archive_node(&mut self, node_id: &str) {
        if let Err(e) = self
            .db
            .update_node_status(node_id, &crate::types::MemoryStatus::Archived)
        {
            tracing::warn!("归档节点 {} 失败: {}", node_id, e);
        }
        self.delete_from_recall_indexes(node_id).await;
    }

    async fn delete_from_recall_indexes(&mut self, node_id: &str) {
        // 从 Tantivy 中删除
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.delete_node(node_id)
        {
            tracing::warn!("从 Tantivy 删除节点 {} 失败（非致命）: {}", node_id, e);
        }
        if let Err(e) = self.recall_engine.delete_node(node_id).await {
            tracing::warn!("从语义向量索引删除节点 {} 失败（非致命）: {}", node_id, e);
        }
        if let Some(ref mut tantivy) = self.tantivy
            && let Err(e) = tantivy.commit()
        {
            tracing::warn!("Tantivy commit 失败（非致命）: {e}");
        }
    }

    /// 设置节点状态，供 GUI 手动管理使用。
    pub(crate) async fn set_node_status(
        &mut self,
        node_id: &str,
        status: MemoryStatus,
    ) -> Result<()> {
        self.db.update_node_status(node_id, &status)?;
        match status {
            MemoryStatus::Archived => {
                self.delete_from_recall_indexes(node_id).await;
            }
            MemoryStatus::Active => {
                if let Some(ref mut tantivy) = self.tantivy
                    && let Some(node) = self.db.get_memory_node(node_id)?
                    && let Err(e) = tantivy.index_node(&node, "")
                {
                    tracing::warn!("恢复 Tantivy 节点 {} 失败（非致命）: {}", node_id, e);
                }
                if let Some(ref mut tantivy) = self.tantivy
                    && let Err(e) = tantivy.commit()
                {
                    tracing::warn!("Tantivy commit 失败（非致命）: {e}");
                }
                if let Some(node) = self.db.get_memory_node(node_id)?
                    && let Err(e) = self.recall_engine.upsert_node(&node).await
                {
                    tracing::warn!("恢复向量索引节点 {} 失败（非致命）: {}", node_id, e);
                }
            }
        }
        Ok(())
    }

    /// 全文搜索（Tantivy BM25，同步版，供兼容使用）
    #[allow(dead_code)]
    pub(crate) fn search(&self, query: &str, limit: usize) -> Vec<RecallHit> {
        self.tantivy
            .as_ref()
            .and_then(|tantivy| tantivy.search(query, limit).ok())
            .unwrap_or_default()
    }

    /// 运行时粗回忆：只使用本地全文搜索，不触发 embedding、rerank 或 Memory LLM。
    /// 结果按 BM25 相关性 × 时间衰减排序，较新记忆获得更高权重。
    pub(crate) fn rough_recall(&self, query: &str, limit: usize) -> Vec<RecallHit> {
        let mut hits = self.search(query, limit);
        if hits.is_empty() {
            return hits;
        }
        self.apply_time_decay(&mut hits);
        hits
    }

    /// 加载最近的活跃记忆节点并转换为 RecallHit。
    pub(crate) fn recent_memory_hits(&self, limit: usize) -> Vec<RecallHit> {
        self.db
            .recent_active_nodes(limit)
            .unwrap_or_default()
            .into_iter()
            .map(|node| RecallHit {
                node_id: node.id,
                title: node.title.clone(),
                summary: compact_text(&node.summary, 200),
                score: 1.0,
                kind: node.kind,
                importance: node.importance as f64,
                depth1_loaded: false,
            })
            .collect()
    }

    /// 对命中结果应用时间衰减排序。
    fn apply_time_decay(&self, hits: &mut [RecallHit]) {
        let node_ids: Vec<&str> = hits.iter().map(|h| h.node_id.as_str()).collect();
        let timestamps = self.db.batch_load_created_at(&node_ids).unwrap_or_default();
        let now = chrono::Local::now().naive_local();
        for hit in hits.iter_mut() {
            if let Some(created_str) = timestamps.get(&hit.node_id)
                && let Ok(created) =
                    chrono::NaiveDateTime::parse_from_str(created_str, "%Y-%m-%d %H:%M:%S")
            {
                let age_days = (now - created).num_seconds() as f64 / 86400.0;
                let decay = (-0.05 * age_days).exp();
                hit.score *= decay;
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
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

fn normalize_keywords(keywords: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for keyword in keywords {
        let keyword = keyword.trim();
        if keyword.is_empty() || normalized.iter().any(|item| item == keyword) {
            continue;
        }
        normalized.push(keyword.to_string());
    }
    normalized
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut clipped = normalized.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}

fn episode_relation_signal(current: &Episode, candidate: &Episode) -> Option<(f32, String)> {
    let shared_keywords = current
        .keywords
        .iter()
        .filter(|keyword| {
            candidate
                .keywords
                .iter()
                .any(|other| keyword.eq_ignore_ascii_case(other))
        })
        .cloned()
        .collect::<Vec<_>>();
    let same_session = current.session_id == candidate.session_id;
    let same_type = current.memory_type == candidate.memory_type;

    if shared_keywords.len() >= 2 {
        let weight = (0.45 + shared_keywords.len() as f32 * 0.08).min(0.85);
        return Some((
            weight,
            format!("自动关联：共享关键词 {}", shared_keywords.join(", ")),
        ));
    }
    if same_session && same_type && current.memory_type != MemoryCognitiveType::Factual {
        return Some((0.55, "自动关联：同一会话中的同类记忆".to_string()));
    }
    None
}

/// 将 Episode 转换为 MemoryNode（用于 Tantivy 索引）
fn episode_to_node(ep: &Episode, workspace_id: Option<&str>) -> MemoryNode {
    let now = chrono::Local::now().naive_local().to_string();
    MemoryNode {
        id: ep.id.clone(),
        kind: MemoryKind::Episode,
        memory_type: ep.memory_type.clone(),
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

fn entity_to_node(entity: &Entity, workspace_id: Option<&str>) -> MemoryNode {
    MemoryNode {
        id: entity.id.clone(),
        kind: MemoryKind::Entity,
        memory_type: MemoryCognitiveType::ProjectStructure,
        scope_type: MemoryScopeType::Workspace,
        scope_id: workspace_id.map(String::from),
        title: entity.name.clone(),
        summary: entity.description.clone(),
        keywords: entity.related_episodes.clone(),
        importance: entity.importance,
        confidence: 1.0,
        status: MemoryStatus::Active,
        source: entity.file_path.clone(),
        usage_count: 0,
        last_used_at: None,
        created_at: entity.created_at.clone(),
        updated_at: entity.updated_at.clone(),
    }
}

fn decision_to_node(decision: &Decision, workspace_id: Option<&str>) -> MemoryNode {
    MemoryNode {
        id: decision.id.clone(),
        kind: MemoryKind::Decision,
        memory_type: MemoryCognitiveType::ArchitectureDecision,
        scope_type: MemoryScopeType::Workspace,
        scope_id: workspace_id.map(String::from),
        title: decision.title.clone(),
        summary: decision.context.clone(),
        keywords: decision.reasons.clone(),
        importance: 0.7,
        confidence: 1.0,
        status: MemoryStatus::Active,
        source: Some(decision.chosen.clone()),
        usage_count: 0,
        last_used_at: None,
        created_at: decision.created_at.clone(),
        updated_at: decision.created_at.clone(),
    }
}

fn memory_base_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tiangong")
        .join("memory")
}

fn default_vector_mode() -> MemoryVectorMode {
    MemoryVectorMode::EmbeddedLanceDb
}

fn memory_index_base_dir(workspace_id: Option<&str>) -> PathBuf {
    let base = memory_base_dir();
    workspace_id
        .filter(|workspace_id| !workspace_id.trim().is_empty())
        .map(|workspace_id| base.join("workspaces").join(workspace_id))
        .unwrap_or(base)
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
