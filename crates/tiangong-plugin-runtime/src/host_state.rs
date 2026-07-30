//! Store 内部的宿主状态。
//!
//! 承载：
//! - WASI 上下文与资源表（满足 WASIp2 组件对基础接口的导入依赖）；
//! - 内存/表/实例上限（[StoreLimits]）；
//! - clock host import（提供真实时间）；
//! - memory-store host import（通用 request，经 [MemoryHandle] 转发到 sidecar）。

use std::time::{SystemTime, UNIX_EPOCH};

use tiangong_memory::MemoryHandle;
use tiangong_memory::ipc::protocol::{MemoryIpcRequestPayload, MemoryIpcResponsePayload};
use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::tiangong::plugin::clock::Host as ClockHost;
use crate::bindings::tiangong::plugin::memory_store::{Host as MemoryStoreHost, MemoryStoreError};

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

/// memory-store host import：通用 request，转发到 sidecar（经 MemoryHandle）。
///
/// WASM 传入的 `payload` 是 `MemoryIpcRequestPayload` 的 JSON 文本
///（含 `method` 内部标签，如 `{"method":"recall",...}`）。host 反序列化后
/// 经 `MemoryHandle::ipc_request` 分发到具体能力，结果序列化回 JSON。
/// handle 缺失时返回 disabled。
impl MemoryStoreHost for HostState {
    fn request(&mut self, method: String, payload: String) -> Result<String, MemoryStoreError> {
        let _ = method; // method 已内含在 payload JSON 的 tag 中，保留参数仅为 WIT 契约清晰。
        let Some(handle) = self.memory.clone() else {
            return Err(MemoryStoreError::Disabled);
        };
        let request_payload: MemoryIpcRequestPayload = serde_json::from_str(&payload)
            .map_err(|e| MemoryStoreError::Message(format!("解析 request payload 失败: {e}")))?;
        let response: MemoryIpcResponsePayload = self
            .runtime
            .handle()
            .block_on(async move { handle.ipc_request(request_payload).await })
            .map_err(|e| MemoryStoreError::Message(format!("{e}")))?;
        serde_json::to_string(&response)
            .map_err(|e| MemoryStoreError::Message(format!("序列化 response 失败: {e}")))
    }
}
