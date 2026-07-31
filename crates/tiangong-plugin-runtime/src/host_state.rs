//! Store 内部的宿主状态。
//!
//! 承载：
//! - WASI 上下文与资源表（满足 WASIp2 组件对基础接口的导入依赖）；
//! - 内存/表/实例上限（[StoreLimits]）；
//! - clock host import（提供真实时间）；
//! - memory-store host import（通用 request，经 [MemoryHandle] 转发到 sidecar）。
//!
//! 插件读写自己的配置经 WASI filesystem（host preopen plugins 目录），不在此处理。

use std::sync::OnceLock;
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
}

impl HostState {
    pub fn new(limits: StoreLimits, memory: Option<MemoryHandle>) -> Self {
        let wasi = build_wasi_ctx();
        Self {
            limits,
            wasi,
            table: ResourceTable::new(),
            memory,
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
impl MemoryStoreHost for HostState {
    fn request(&mut self, method: String, payload: String) -> Result<String, MemoryStoreError> {
        let _ = method;
        let Some(handle) = self.memory.clone() else {
            return Err(MemoryStoreError::Disabled);
        };
        let request_payload: MemoryIpcRequestPayload = serde_json::from_str(&payload)
            .map_err(|e| MemoryStoreError::Message(format!("解析 request payload 失败: {e}")))?;
        let response: MemoryIpcResponsePayload = crate::execution::run_outside_tokio(move || {
            memory_runtime()?.block_on(async move { handle.ipc_request(request_payload).await })
        })
        .map_err(|e| MemoryStoreError::Message(e.to_string()))?;
        serde_json::to_string(&response)
            .map_err(|e| MemoryStoreError::Message(format!("序列化 response 失败: {e}")))
    }
}

/// 全部 WASM 实例共用一个 runtime，避免每个会话创建一组 Tokio worker 线程。
fn memory_runtime() -> anyhow::Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("创建 memory host runtime 失败: {e}"))
}

/// 构建 WASI 上下文，preopen 插件配置目录供 WASM 组件用 std::fs 读写自己的配置。
fn build_wasi_ctx() -> WasiCtx {
    let mut builder = WasiCtxBuilder::new();
    // preopen ~/.tiangong/plugins/memory/ 目录，映射为 WASM 内的当前目录。
    // 插件用 std::fs::read_to_string("config.json") 读写自己的配置。
    let plugin_config_dir = plugin_config_dir();
    let _ = std::fs::create_dir_all(&plugin_config_dir);
    if let Err(e) = builder.preopened_dir(
        &plugin_config_dir,
        ".",
        wasmtime_wasi::DirPerms::all(),
        wasmtime_wasi::FilePerms::all(),
    ) {
        tracing::debug!("preopen 插件配置目录失败（插件配置读写将不可用）: {e}");
    }
    builder.build()
}

/// 插件配置目录：~/.tiangong/plugins/memory/
fn plugin_config_dir() -> std::path::PathBuf {
    fn user_home() -> Option<std::path::PathBuf> {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            return Some(std::path::PathBuf::from(home));
        }
        std::env::var_os("USERPROFILE")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
    }
    user_home()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join(".tiangong")
        .join("plugins")
        .join("memory")
}
