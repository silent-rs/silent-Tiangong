//! Store 内部的宿主状态。
//!
//! 承载：
//! - WASI 上下文与资源表（满足 WASIp2 组件对基础接口的导入依赖）；
//! - 内存/表/实例上限（[StoreLimits]）；
//! - clock host import（提供真实时间）；
//! - memory-store host import（经 [MemoryHandle] 查询真实记忆）。
//!
//! 阶段三：memory-store 的 recall 在独立多线程 tokio runtime 上 block_on
//! 调用 [MemoryHandle] 的 async 方法，桥接 WASM 同步调用栈与宿主 async 存储。

use std::time::{SystemTime, UNIX_EPOCH};

use tiangong_memory::types::{
    MemoryKind as HostMemoryKind, RecallAnchors, RecallHit as HostRecallHit,
};
use tiangong_memory::{MemoryHandle, SearchStrategy as HostSearchStrategy};
use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::tiangong::plugin::clock::Host as ClockHost;
use crate::bindings::tiangong::plugin::memory_store::{
    Host as MemoryStoreHost, MemoryStoreError, RecallHit, RecallResponse,
};
use crate::bindings::tiangong::plugin::plugin::MemoryKind;

/// WASM Store 的宿主侧状态。
pub struct HostState {
    limits: StoreLimits,
    wasi: WasiCtx,
    table: ResourceTable,
    /// 记忆句柄，None 时 memory-store import 返回 disabled。
    memory: Option<MemoryHandle>,
    /// 用于 block_on MemoryHandle async 方法的多线程 runtime。
    runtime: tokio::runtime::Runtime,
}

impl HostState {
    pub fn new(limits: StoreLimits, memory: Option<MemoryHandle>) -> Self {
        let wasi = WasiCtxBuilder::new().build();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建多线程 tokio runtime 失败");
        Self {
            limits,
            wasi,
            table: ResourceTable::new(),
            memory,
            runtime,
        }
    }

    /// 提供对内部限制器的可变借用，供 `Store::limiter` 闭包返回。
    pub fn limits_mut(&mut self) -> &mut StoreLimits {
        &mut self.limits
    }
}

/// 让 wasmtime-wasi 经由该状态访问 WASI 上下文与资源表。
impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// clock host import：返回自 UNIX epoch 起的毫秒数。
impl ClockHost for HostState {
    fn now_millis(&mut self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// memory-store host import：经 MemoryHandle 执行 BM25 粗召回。
///
/// WASM 同步调用栈内，在多线程 runtime 上 block_on async 召回，阻塞当前线程
/// 直到 Actor 返回。handle 缺失时返回 disabled，由 WASM 侧回退到 mock。
impl MemoryStoreHost for HostState {
    fn recall(
        &mut self,
        query: String,
        keywords: Vec<String>,
        limit: u32,
    ) -> Result<RecallResponse, MemoryStoreError> {
        let Some(handle) = self.memory.clone() else {
            return Err(MemoryStoreError::Disabled);
        };
        let anchors = RecallAnchors {
            query,
            keywords,
            strategy: Some(HostSearchStrategy::Keyword),
        };
        // 在多线程 runtime 上 block_on async 召回。
        let hits = self
            .runtime
            .handle()
            .block_on(async move { handle.recall(anchors, limit as usize).await });
        Ok(RecallResponse {
            content: String::new(),
            hits: hits.into_iter().map(RecallHit::from).collect(),
            used_llm: false,
        })
    }
}

/// 宿主侧 RecallHit → WIT RecallHit。
impl From<HostRecallHit> for RecallHit {
    fn from(h: HostRecallHit) -> Self {
        Self {
            node_id: h.node_id,
            title: h.title,
            summary: h.summary,
            score: h.score,
            kind: h.kind.into(),
            importance: h.importance,
            depth1_loaded: h.depth1_loaded,
        }
    }
}

/// 宿主侧 MemoryKind → WIT MemoryKind。
impl From<HostMemoryKind> for MemoryKind {
    fn from(k: HostMemoryKind) -> Self {
        match k {
            HostMemoryKind::Episode => Self::Episode,
            HostMemoryKind::Entity => Self::Entity,
            HostMemoryKind::Decision => Self::Decision,
            HostMemoryKind::Evidence => Self::Evidence,
        }
    }
}
