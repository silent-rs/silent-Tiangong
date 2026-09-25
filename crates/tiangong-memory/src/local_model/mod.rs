//! 内置本地推理（`builtin` 来源的 Embedding / Rerank）。
//!
//! - [`catalog`]：各档位固定的模型清单（revision + sha256）；
//! - [`download`]：断点续传下载与校验；
//! - [`provider`]：基于 fastembed 的推理实现。
//!
//! 选择内置来源后，Memory Actor 在后台下载并加载模型，就绪前召回按
//! BM25（及其他已就绪组件）降级运行，就绪后自动启用。

pub(crate) mod catalog;
pub(crate) mod download;
pub(crate) mod provider;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use serde::Serialize;
use tiangong_llm::{EmbeddingProvider, RerankProvider};

pub use download::MODEL_MIRROR_ENV;

use crate::config::MemoryLocalTier;
use catalog::{LocalModelKind, LocalModelSpec};

/// 本进程内的加载状态（下载进度另从磁盘读取，跨进程可见）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeState {
    Downloading,
    Loading,
    Ready,
    Failed(String),
}

fn runtime_states() -> &'static Mutex<HashMap<&'static str, RuntimeState>> {
    static STATES: OnceLock<Mutex<HashMap<&'static str, RuntimeState>>> = OnceLock::new();
    STATES.get_or_init(Default::default)
}

fn set_state(spec: &LocalModelSpec, state: RuntimeState) {
    if let Ok(mut states) = runtime_states().lock() {
        states.insert(spec.id, state);
    }
}

fn runtime_state(spec: &LocalModelSpec) -> Option<RuntimeState> {
    runtime_states()
        .lock()
        .ok()
        .and_then(|states| states.get(spec.id).cloned())
}

/// 下载（如需）并加载本地 Embedding 模型。
pub(crate) async fn load_embedding(tier: MemoryLocalTier) -> Result<Arc<dyn EmbeddingProvider>> {
    let spec = catalog::embedding_model(tier);
    let dir = prepare(spec).await?;
    let loaded = load_blocking(spec, move || {
        provider::LocalEmbeddingProvider::load(&dir, spec)
    })
    .await?;
    Ok(Arc::new(loaded))
}

/// 下载（如需）并加载本地 Rerank 模型。
pub(crate) async fn load_rerank(tier: MemoryLocalTier) -> Result<Arc<dyn RerankProvider>> {
    let spec = catalog::rerank_model(tier);
    let dir = prepare(spec).await?;
    let loaded = load_blocking(spec, move || {
        provider::LocalRerankProvider::load(&dir, spec)
    })
    .await?;
    Ok(Arc::new(loaded))
}

/// 集成测试入口：下载并加载内置 Embedding（真实模型冒烟用）。
#[doc(hidden)]
pub async fn load_embedding_for_test(tier: MemoryLocalTier) -> Result<Arc<dyn EmbeddingProvider>> {
    load_embedding(tier).await
}

/// 集成测试入口：下载并加载内置 Rerank。
#[doc(hidden)]
pub async fn load_rerank_for_test(tier: MemoryLocalTier) -> Result<Arc<dyn RerankProvider>> {
    load_rerank(tier).await
}

async fn prepare(spec: &'static LocalModelSpec) -> Result<std::path::PathBuf> {
    let root = download::models_dir();
    if !download::is_installed(&root, spec) {
        set_state(spec, RuntimeState::Downloading);
        tracing::info!(
            model = spec.id,
            size_mb = spec.total_size() / 1_000_000,
            "Memory 内置模型未下载，开始后台下载"
        );
    }
    match download::ensure_model(&root, spec).await {
        Ok(dir) => Ok(dir),
        Err(error) => {
            set_state(spec, RuntimeState::Failed(format!("下载失败：{error:#}")));
            Err(error)
        }
    }
}

async fn load_blocking<T, F>(spec: &'static LocalModelSpec, load: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    set_state(spec, RuntimeState::Loading);
    let result = tokio::task::spawn_blocking(load)
        .await
        .map_err(|error| anyhow::anyhow!("加载本地模型任务异常退出: {error}"))
        .and_then(|result| result);
    match &result {
        Ok(_) => {
            set_state(spec, RuntimeState::Ready);
            tracing::info!(model = spec.id, "Memory 内置模型已加载");
        }
        Err(error) => set_state(spec, RuntimeState::Failed(format!("加载失败：{error:#}"))),
    }
    result
}

/// 页面展示用的本地模型状态。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LocalModelStatus {
    /// low | mid | high
    pub tier: String,
    /// embedding | rerank
    pub kind: String,
    pub model: String,
    /// 向量维度（仅 embedding）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
    /// 全部文件总字节数。
    pub size: u64,
    /// not_downloaded | downloading | loading | ready | installed | failed
    pub state: String,
    /// 已下载字节数（下载中时有意义）。
    pub downloaded: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 全部档位的本地模型状态。
pub fn local_model_statuses() -> Vec<LocalModelStatus> {
    let root = download::models_dir();
    let mut statuses = Vec::new();
    for tier in [
        MemoryLocalTier::Low,
        MemoryLocalTier::Mid,
        MemoryLocalTier::High,
    ] {
        for kind in [LocalModelKind::Embedding, LocalModelKind::Rerank] {
            statuses.push(status_of(&root, tier, catalog::model_for(kind, tier)));
        }
    }
    statuses
}

/// 指定档位与用途的模型状态。
pub fn local_model_status(tier: MemoryLocalTier, embedding: bool) -> LocalModelStatus {
    let kind = if embedding {
        LocalModelKind::Embedding
    } else {
        LocalModelKind::Rerank
    };
    status_of(
        &download::models_dir(),
        tier,
        catalog::model_for(kind, tier),
    )
}

fn status_of(root: &Path, tier: MemoryLocalTier, spec: &LocalModelSpec) -> LocalModelStatus {
    let installed = download::is_installed(root, spec);
    let downloaded = if installed {
        spec.total_size()
    } else {
        download::partial_bytes(root, spec)
    };
    let (state, error) = match runtime_state(spec) {
        Some(RuntimeState::Ready) => ("ready", None),
        Some(RuntimeState::Loading) => ("loading", None),
        Some(RuntimeState::Failed(message)) => ("failed", Some(message)),
        _ if installed => ("installed", None),
        Some(RuntimeState::Downloading) => ("downloading", None),
        // 其他进程（如 Leader）正在下载：以磁盘上的临时文件判断。
        None if downloaded > 0 => ("downloading", None),
        None => ("not_downloaded", None),
    };
    LocalModelStatus {
        tier: tier.key().to_string(),
        kind: spec.kind.key().to_string(),
        model: spec.id.to_string(),
        dimension: (spec.dimension > 0).then_some(spec.dimension),
        size: spec.total_size(),
        state: state.to_string(),
        downloaded,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_cover_every_tier_and_kind() {
        let statuses = local_model_statuses();
        assert_eq!(statuses.len(), 6);
        let mid_embedding = statuses
            .iter()
            .find(|status| status.tier == "mid" && status.kind == "embedding")
            .expect("中档 embedding");
        assert_eq!(mid_embedding.model, "bge-base-zh-v1.5");
        assert_eq!(mid_embedding.dimension, Some(768));
        let high_rerank = local_model_status(MemoryLocalTier::High, false);
        assert_eq!(high_rerank.model, "bge-reranker-v2-m3");
        assert_eq!(high_rerank.dimension, None);
    }

    #[test]
    fn failure_state_is_reported() {
        let spec = &catalog::BGE_RERANKER_V2_M3;
        set_state(spec, RuntimeState::Failed("下载失败：network".into()));
        let status = local_model_status(MemoryLocalTier::High, false);
        assert_eq!(status.state, "failed");
        assert_eq!(status.error.as_deref(), Some("下载失败：network"));
        runtime_states().lock().unwrap().remove(spec.id);
    }
}
