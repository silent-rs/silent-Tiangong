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

use serde::Serialize;
use tiangong_memory::MemoryHandle;
use tiangong_memory::ipc::protocol::{MemoryIpcRequestPayload, MemoryIpcResponsePayload};
use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::tiangong::plugin::clock::Host as ClockHost;
use crate::bindings::tiangong::plugin::memory_store::{Host as MemoryStoreHost, MemoryStoreError};

const UI_CONFIG_GET: &str = "ui.memory.config.get";
const UI_CONFIG_SET: &str = "ui.memory.config.set";
const UI_MEMORY_REQUEST: &str = "ui.memory.request";

#[derive(Serialize)]
struct MemoryUiBootstrap {
    config: tiangong_memory::MemoryConfigSelection,
    models: Vec<MemoryUiModel>,
}

#[derive(Serialize)]
struct MemoryUiModel {
    key: String,
    provider: String,
    model: String,
    capabilities: Vec<String>,
    dimension: Option<usize>,
}

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
        match method.as_str() {
            UI_CONFIG_GET => return memory_ui_bootstrap(),
            UI_CONFIG_SET => return self.save_memory_ui_config(&payload),
            UI_MEMORY_REQUEST => return self.forward_ui_memory_request(&payload),
            _ => {}
        }

        let handle = self.memory.clone().ok_or(MemoryStoreError::Disabled)?;
        let request_payload: MemoryIpcRequestPayload = serde_json::from_str(&payload)
            .map_err(|e| MemoryStoreError::Message(format!("解析 request payload 失败: {e}")))?;
        forward_memory_request(handle, request_payload)
    }
}

impl HostState {
    fn save_memory_ui_config(&mut self, payload: &str) -> Result<String, MemoryStoreError> {
        let models = tiangong_config::registry::try_models()
            .ok_or_else(|| MemoryStoreError::Message("主模型配置尚未初始化".to_string()))?;
        let selection: tiangong_memory::MemoryConfigSelection = serde_json::from_str(payload)
            .map_err(|e| MemoryStoreError::Message(format!("解析 Memory 配置失败: {e}")))?;
        let config = selection
            .to_memory(&models)
            .map_err(|e| MemoryStoreError::Message(e.to_string()))?;
        config
            .save()
            .map_err(|e| MemoryStoreError::Message(format!("保存 Memory 配置失败: {e}")))?;

        let previous = self.memory.clone();
        let handle = match previous {
            Some(handle) => Some(handle),
            None => refresh_memory_handle()?,
        };
        if let Some(handle) = handle {
            reconfigure_memory_handle(handle.clone(), config.to_options())?;
            self.memory = Some(handle);
        } else if !tiangong_memory::is_memory_disabled() {
            return Err(MemoryStoreError::Message(
                "Memory 配置已保存，但运行实例初始化失败".to_string(),
            ));
        }

        Ok(r#"{"ok":true}"#.to_string())
    }

    fn forward_ui_memory_request(&mut self, payload: &str) -> Result<String, MemoryStoreError> {
        let request_payload: MemoryIpcRequestPayload = serde_json::from_str(payload)
            .map_err(|e| MemoryStoreError::Message(format!("解析 Memory 页面请求失败: {e}")))?;
        let handle = self.ensure_memory_handle()?;
        forward_memory_request(handle, request_payload)
    }

    fn ensure_memory_handle(&mut self) -> Result<MemoryHandle, MemoryStoreError> {
        if let Some(handle) = self.memory.clone() {
            return Ok(handle);
        }
        let handle = refresh_memory_handle()?.ok_or(MemoryStoreError::Disabled)?;
        self.memory = Some(handle.clone());
        Ok(handle)
    }
}

fn memory_ui_bootstrap() -> Result<String, MemoryStoreError> {
    let models = tiangong_config::registry::try_models()
        .ok_or_else(|| MemoryStoreError::Message("主模型配置尚未初始化".to_string()))?;
    let config = tiangong_memory::MemoryConfig::load_or_default();
    let selection = tiangong_memory::MemoryConfigSelection::from_memory(&config, &models);
    let mut model_entries = models
        .models
        .iter()
        .map(|(key, entry)| MemoryUiModel {
            key: key.clone(),
            provider: entry.provider.clone(),
            model: entry.model.clone(),
            capabilities: entry
                .capabilities
                .iter()
                .map(|capability| capability.key().to_string())
                .collect(),
            dimension: entry
                .options
                .get("dimension")
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok()),
        })
        .collect::<Vec<_>>();
    model_entries.sort_by(|left, right| left.key.cmp(&right.key));

    serde_json::to_string(&MemoryUiBootstrap {
        config: selection,
        models: model_entries,
    })
    .map_err(|e| MemoryStoreError::Message(format!("序列化 Memory 页面配置失败: {e}")))
}

fn refresh_memory_handle() -> Result<Option<MemoryHandle>, MemoryStoreError> {
    crate::execution::run_outside_tokio(|| {
        Ok(memory_runtime()?
            .block_on(async { tiangong_memory::registry::get_or_init_memory_handle_async().await }))
    })
    .map_err(|e| MemoryStoreError::Message(format!("初始化 Memory 失败: {e}")))
}

fn reconfigure_memory_handle(
    handle: MemoryHandle,
    options: tiangong_memory::MemoryOptions,
) -> Result<(), MemoryStoreError> {
    crate::execution::run_outside_tokio(move || {
        memory_runtime()?.block_on(async move { handle.reconfigure(options).await })
    })
    .map_err(|e| MemoryStoreError::Message(format!("应用 Memory 配置失败: {e}")))
}

fn forward_memory_request(
    handle: MemoryHandle,
    request_payload: MemoryIpcRequestPayload,
) -> Result<String, MemoryStoreError> {
    let response: MemoryIpcResponsePayload = crate::execution::run_outside_tokio(move || {
        memory_runtime()?.block_on(async move { handle.ipc_request(request_payload).await })
    })
    .map_err(|e| MemoryStoreError::Message(e.to_string()))?;
    serde_json::to_string(&response)
        .map_err(|e| MemoryStoreError::Message(format!("序列化 response 失败: {e}")))
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
