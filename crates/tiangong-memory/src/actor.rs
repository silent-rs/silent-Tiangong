//! Memory Actor — 独立 tokio task 运行时
//!
//! 所有 Memory 读写均串行经过 Actor，天然消除并发冲突。
//! 外部通过 MemoryHandle 的 mpsc channel 发送命令。

use tokio::sync::mpsc;

use crate::command::MemoryCommand;
use crate::handle::MemoryHandle;
use crate::options::MemoryOptions;
use crate::recall_context;
use crate::rumination;
use crate::store::MemoryStore;
use crate::types::{EnhancedTurnResult, MemoryCandidate};

struct RuminationJob {
    turn_result: EnhancedTurnResult,
    model: Option<tiangong_llm::LlmEndpointConfig>,
}

/// Memory Actor（独立运行时）
pub(crate) struct MemoryActor {
    rx: mpsc::Receiver<MemoryCommand>,
    store: MemoryStore,
    options: MemoryOptions,
    pending_candidates: Vec<MemoryCandidate>,
    rumination_tx: mpsc::Sender<RuminationJob>,
}

impl MemoryActor {
    fn new(
        rx: mpsc::Receiver<MemoryCommand>,
        store: MemoryStore,
        options: MemoryOptions,
        rumination_tx: mpsc::Sender<RuminationJob>,
    ) -> Self {
        Self {
            rx,
            store,
            options,
            pending_candidates: Vec::new(),
            rumination_tx,
        }
    }

    /// 启动 Actor 消息循环
    pub(crate) async fn run(mut self) {
        self.store
            .try_enable_recall_engine(
                self.options.embedding.as_ref(),
                self.options.rerank.as_ref(),
                self.options.vector_mode,
            )
            .await;
        tracing::info!("Memory Actor 已启动");
        loop {
            match self.rx.recv().await {
                Some(MemoryCommand::Shutdown) => {
                    tracing::info!("Memory Actor 收到 Shutdown 命令，正在关闭");
                    break;
                }
                Some(cmd) => self.handle(cmd).await,
                None => {
                    tracing::info!("Memory Actor 通道已关闭，退出");
                    break;
                }
            }
        }
        tracing::info!("Memory Actor 已关闭");
    }

    async fn handle(&mut self, cmd: MemoryCommand) {
        match cmd {
            MemoryCommand::LoadInjection {
                session_id,
                workspace_id,
                reply,
            } => {
                let result = self
                    .store
                    .load_injection(&session_id, workspace_id.as_deref());
                let _ = reply.send(result);
            }

            MemoryCommand::WriteEpisode {
                episode,
                workspace_id,
            } => {
                if let Err(e) = self
                    .store
                    .write_episode(episode, workspace_id.as_deref())
                    .await
                {
                    tracing::warn!("Memory 写入 Episode 失败: {}", e);
                }
            }

            MemoryCommand::UpdateInjection {
                level,
                target_id,
                content,
            } => {
                if let Err(e) = self.store.update_injection(level, &target_id, &content) {
                    tracing::warn!("Memory 更新 Injection 失败: {}", e);
                }
            }

            MemoryCommand::UpsertManualMemory { draft, reply } => {
                let result = if draft.id.as_deref().is_some_and(|id| !id.trim().is_empty()) {
                    self.store.update_manual_memory(draft).await
                } else {
                    self.store.upsert_manual_memory(draft).await
                }
                .map_err(|err| err.to_string());
                let _ = reply.send(result);
            }

            MemoryCommand::SetNodeStatus {
                node_id,
                status,
                reply,
            } => {
                let result = self
                    .store
                    .set_node_status(&node_id, status)
                    .await
                    .map_err(|err| err.to_string());
                let _ = reply.send(result);
            }

            MemoryCommand::UpsertRelation { draft, reply } => {
                let result = self
                    .store
                    .upsert_relation(draft)
                    .map_err(|err| err.to_string());
                let _ = reply.send(result);
            }

            MemoryCommand::DeleteRelation { relation_id, reply } => {
                let result = self
                    .store
                    .delete_relation(&relation_id)
                    .map_err(|err| err.to_string());
                let _ = reply.send(result);
            }

            // Phase C：双引擎召回（Tantivy BM25 + LanceDB 语义）
            MemoryCommand::Recall {
                anchors,
                limit,
                reply,
            } => {
                let hits = self.store.recall_async(&anchors, limit).await;
                let _ = reply.send(hits);
            }

            MemoryCommand::RecallContext { request, reply } => {
                let response = recall_context::recall_context(
                    &self.store,
                    self.options.model.as_ref(),
                    request,
                )
                .await;
                let _ = reply.send(response);
            }

            MemoryCommand::RoughRecall { context, reply } => {
                let limit = context.policy.rough_limit.clamp(1, 10);

                // 首条消息的运行时回忆：优先使用 LLM 规划检索策略
                let hits = if let Some(strategy) = recall_context::plan_runtime_recall(
                    &context.query,
                    &context.current_context,
                    self.options.model.as_ref(),
                )
                .await
                {
                    match strategy {
                        recall_context::RuntimeRecallStrategy::Recent => {
                            let recent = self.store.recent_memory_hits(limit);
                            tracing::debug!(
                                query = %context.query,
                                recent_count = recent.len(),
                                "Memory 运行时粗回忆使用 LLM 规划的最近记忆策略"
                            );
                            recent
                        }
                        recall_context::RuntimeRecallStrategy::Keyword { search_terms } => {
                            let expanded_query = search_terms.join(" ");
                            let hits = self.store.rough_recall(&expanded_query, limit);
                            tracing::debug!(
                                query = %context.query,
                                expanded_query = %expanded_query,
                                expanded_count = hits.len(),
                                "Memory 运行时粗回忆使用 LLM 规划的扩展关键词策略"
                            );
                            if hits.is_empty() {
                                self.store.recent_memory_hits(limit)
                            } else {
                                hits
                            }
                        }
                    }
                } else {
                    // 无 LLM 或规划失败，直接 BM25
                    let hits = self.store.rough_recall(&context.query, limit);
                    tracing::debug!(
                        query = %context.query,
                        trigger = ?context.trigger,
                        hit_count = hits.len(),
                        "Memory 运行时粗回忆 BM25 fallback"
                    );
                    hits
                };

                let _ = reply.send(hits);
            }

            MemoryCommand::EvaluateRecallSufficiency {
                context,
                rough_hits,
                reply,
            } => {
                let result = recall_context::evaluate_recall_sufficiency(
                    &context,
                    &rough_hits,
                    self.options.model.as_ref(),
                )
                .await;
                tracing::debug!(
                    query = %context.query,
                    sufficient = result.sufficient,
                    should_upgrade_to_hybrid = result.should_upgrade_to_hybrid,
                    "Memory 运行时召回充分性评估完成"
                );
                let _ = reply.send(result);
            }

            MemoryCommand::LoadDepth2 { node_ids, reply } => {
                let items = self.store.load_depth2(&node_ids);
                let _ = reply.send(items);
            }

            MemoryCommand::ListNodes { query, reply } => {
                let items = self.store.list_nodes(&query);
                let _ = reply.send(items);
            }

            MemoryCommand::CountNodes { query, reply } => {
                let count = self.store.count_nodes(&query);
                let _ = reply.send(count);
            }

            MemoryCommand::ListRelations { node_id, reply } => {
                let items = self.store.list_relations(&node_id);
                let _ = reply.send(items);
            }

            MemoryCommand::ListRelationsBatch { node_ids, reply } => {
                let items = self.store.list_relations_batch(&node_ids);
                let _ = reply.send(items);
            }

            // Phase B：Micro 反刍
            MemoryCommand::SubmitCandidate { candidate } => {
                self.pending_candidates.push(candidate);
            }

            MemoryCommand::RunMicroRumination { turn_result } => {
                // 使用 turn 携带的 workspace_id 作为 scope 标记
                let wid = turn_result.workspace_id.as_deref();
                if let Err(e) = rumination::process_micro(
                    &mut self.store,
                    &turn_result,
                    wid,
                    self.options.model.as_ref(),
                )
                .await
                {
                    tracing::warn!("Micro 反刍失败: {}", e);
                }
            }

            MemoryCommand::EnqueueEnhancedMicroRumination { turn_result, reply } => {
                let started = std::time::Instant::now();
                let mut enhanced = *turn_result;
                let pending = std::mem::take(&mut self.pending_candidates);
                enhanced.memory_candidates.extend(pending.iter().cloned());
                let session_id = enhanced.session_id.clone();
                let turn_id = enhanced.turn_id.clone();
                let job = RuminationJob {
                    turn_result: enhanced,
                    model: self.options.model.clone(),
                };
                let result = self.rumination_tx.try_send(job).map_err(|error| {
                    self.pending_candidates = pending;
                    format!("Memory 反刍队列不可用: {error}")
                });
                match &result {
                    Ok(()) => tracing::debug!(
                        %session_id,
                        %turn_id,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "Memory 增强反刍已入队"
                    ),
                    Err(error) => tracing::warn!(
                        %session_id,
                        %turn_id,
                        %error,
                        "Memory 增强反刍入队失败"
                    ),
                }
                let _ = reply.send(result);
            }
            MemoryCommand::ApplyEnhancedMicroRumination {
                turn_result,
                extraction,
            } => {
                let started = std::time::Instant::now();
                let enhanced = *turn_result;
                let session_id = enhanced.session_id.clone();
                let turn_id = enhanced.turn_id.clone();
                let wid = enhanced.workspace_id.as_deref();
                match rumination::apply_enhanced_micro(&mut self.store, &enhanced, &extraction, wid)
                    .await
                {
                    Ok(()) => tracing::info!(
                        %session_id,
                        %turn_id,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "Memory 增强反刍写入完成"
                    ),
                    Err(error) => tracing::warn!(
                        %session_id,
                        %turn_id,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        %error,
                        "Memory 增强反刍写入失败"
                    ),
                }
            }

            // Phase C 实现
            MemoryCommand::RunMesoRumination {
                session_id,
                workspace_id,
            } => {
                if let Err(e) = rumination::process_meso(
                    &mut self.store,
                    &session_id,
                    &workspace_id,
                    self.options.model.as_ref(),
                )
                .await
                {
                    tracing::warn!("Meso 反刍失败: {}", e);
                }
            }

            // Phase D 实现
            MemoryCommand::RunMetaRumination => {
                if let Err(e) = rumination::process_meta(&mut self.store).await {
                    tracing::warn!("Meta 反刍失败: {}", e);
                }
            }

            MemoryCommand::Reconfigure { options, reply } => {
                let result = self
                    .reconfigure(*options)
                    .await
                    .map_err(|err| err.to_string());
                let _ = reply.send(result);
            }

            // Shutdown 在外层 loop 处理
            MemoryCommand::Shutdown => {}
        }
    }

    async fn reconfigure(&mut self, options: MemoryOptions) -> anyhow::Result<()> {
        self.store
            .reconfigure_recall_engine(
                options.embedding.as_ref(),
                options.rerank.as_ref(),
                options.vector_mode,
            )
            .await;
        self.options = options;
        tracing::info!("Memory Actor 配置热更新完成");
        Ok(())
    }
}

/// 启动 Memory 系统，返回 MemoryHandle（同步接口）
///
/// 内部在独立线程 + current_thread runtime + LocalSet 中运行 Actor，
/// 不要求 MemoryActor 或 MemoryStore 实现 Send/Sync。
pub fn start_memory() -> anyhow::Result<MemoryHandle> {
    start_memory_with_options(MemoryOptions::new())
}
///
/// 上层可通过 `tiangong-config` 读取配置文件，再将解析后的 embedding
/// 端点传入这里。Memory 自身不负责重复解析全局配置文件。
pub fn start_memory_with_options(options: MemoryOptions) -> anyhow::Result<MemoryHandle> {
    let (handle, _join) = spawn_memory_actor(options)?;
    Ok(handle)
}

/// 启动 Memory Actor 线程，返回 handle 与 actor 线程的 JoinHandle。
///
/// 调用方负责在合适的时机 join 返回的 JoinHandle，以保证 actor 线程内的
/// native 资源（Tantivy mmap、SQLite 连接等）在进程退出前完成析构，避免
/// 与进程级全局静态析构产生竞态（Linux 偶发 SIGSEGV）。不需要显式 join
/// 的调用方（如生产路径）可直接丢弃返回的 JoinHandle，actor 线程会在
/// handle 全部释放后随通道关闭自行退出。
fn spawn_rumination_worker(
    mut rx: mpsc::Receiver<RuminationJob>,
    actor_tx: mpsc::WeakSender<MemoryCommand>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("tiangong-memory-rumination".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Memory 反刍 runtime 构建失败");
            runtime.block_on(async move {
                while let Some(job) = rx.recv().await {
                    let started = std::time::Instant::now();
                    let session_id = job.turn_result.session_id.clone();
                    let turn_id = job.turn_result.turn_id.clone();
                    let extraction =
                        rumination::extract_enhanced_micro(&job.turn_result, job.model.as_ref())
                            .await;
                    tracing::info!(
                        %session_id,
                        %turn_id,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        episode_count = extraction.episodes.len(),
                        entity_count = extraction.entities.len(),
                        decision_count = extraction.decisions.len(),
                        evidence_count = extraction.evidences.len(),
                        "Memory 增强反刍提取完成"
                    );
                    let Some(actor_tx) = actor_tx.upgrade() else {
                        tracing::debug!("Memory Actor 已关闭，停止反刍 worker");
                        break;
                    };
                    if actor_tx
                        .send(MemoryCommand::ApplyEnhancedMicroRumination {
                            turn_result: Box::new(job.turn_result),
                            extraction: Box::new(extraction),
                        })
                        .await
                        .is_err()
                    {
                        tracing::debug!("Memory Actor 已关闭，停止反刍 worker");
                        break;
                    }
                }
            });
        })
        .map_err(Into::into)
}

pub(crate) fn spawn_memory_actor(
    options: MemoryOptions,
) -> anyhow::Result<(MemoryHandle, std::thread::JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel(256);
    let (rumination_tx, rumination_rx) = mpsc::channel(64);

    let store = MemoryStore::open().map_err(|e| {
        tracing::error!("Memory Store 初始化失败: {}", e);
        e
    })?;

    let actor = MemoryActor::new(rx, store, options.clone(), rumination_tx);
    let _rumination_join = spawn_rumination_worker(rumination_rx, tx.downgrade())?;

    let join_handle = std::thread::Builder::new()
        .name("tiangong-memory-actor".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Memory Actor runtime 构建失败");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, actor.run());
        })
        .expect("Memory Actor 线程创建失败");

    Ok((MemoryHandle::new(tx), join_handle))
}
