//! Store 内部的宿主状态。
//!
//! 承载：
//! - WASI 上下文与资源表；
//! - 内存/表/实例上限；
//! - clock host import；
//! - sidecar host import（通用 JSON 转发，不理解业务）。
//!
//! 通用运行时不依赖任何具体插件。SidecarConnection 由入口侧注入。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::tiangong::plugin::clock::Host as ClockHost;
use crate::bindings::tiangong::plugin::sidecar::{Host as SidecarHost, SidecarError};
use crate::sidecar::{SidecarConnection, SidecarInvokeError};

/// WASM Store 的宿主侧状态。
pub struct HostState {
    limits: StoreLimits,
    wasi: WasiCtx,
    table: ResourceTable,
    /// sidecar 连接（通用，由入口侧注入）。None 时 invoke 返回 unavailable。
    sidecar: Option<Arc<dyn SidecarConnection>>,
    /// 插件 ID（构造时用于 preopen 插件配置目录）。
    #[allow(dead_code)]
    plugin_id: String,
}

impl HostState {
    pub fn new(
        limits: StoreLimits,
        sidecar: Option<Arc<dyn SidecarConnection>>,
        plugin_id: String,
    ) -> Self {
        let wasi = build_wasi_ctx(&plugin_id);
        Self {
            limits,
            wasi,
            table: ResourceTable::new(),
            sidecar,
            plugin_id,
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

/// clock host import。
impl ClockHost for HostState {
    fn now_millis(&mut self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// sidecar host import：通用 JSON 转发。
///
/// WASM 组件经 `sidecar.invoke(operation, payload)` 调用，Host 负责通用协议封装，
/// 不理解操作名与负载的业务含义。
impl SidecarHost for HostState {
    fn invoke(&mut self, operation: String, payload: String) -> Result<String, SidecarError> {
        let Some(conn) = self.sidecar.clone() else {
            return Err(SidecarError::NotConfigured);
        };
        // 在独立 OS 线程上执行，避免 tokio worker 线程嵌套。
        std::thread::scope(|s| {
            s.spawn(move || conn.invoke(&operation, &payload))
                .join()
                .map_err(|e| SidecarError::Internal(format!("sidecar 线程异常: {e:?}")))?
                .map_err(map_sidecar_error)
        })
    }
}

fn map_sidecar_error(error: anyhow::Error) -> SidecarError {
    match error.downcast_ref::<SidecarInvokeError>() {
        Some(SidecarInvokeError::Unavailable(message)) => {
            SidecarError::Unavailable(message.clone())
        }
        Some(SidecarInvokeError::Timeout) => SidecarError::Timeout,
        Some(SidecarInvokeError::PermissionDenied) => SidecarError::PermissionDenied,
        Some(SidecarInvokeError::ProtocolMismatch(message)) => {
            SidecarError::ProtocolMismatch(message.clone())
        }
        Some(SidecarInvokeError::Internal(message)) => SidecarError::Internal(message.clone()),
        None => SidecarError::Internal(error.to_string()),
    }
}

/// 构建 WASI 上下文，preopen 插件配置目录供 WASM 组件读写自己的配置。
fn build_wasi_ctx(plugin_id: &str) -> WasiCtx {
    let mut builder = WasiCtxBuilder::new();
    let dir = plugin_config_dir(plugin_id);
    let _ = std::fs::create_dir_all(&dir);
    if let Err(e) = builder.preopened_dir(
        &dir,
        ".",
        wasmtime_wasi::DirPerms::all(),
        wasmtime_wasi::FilePerms::all(),
    ) {
        tracing::debug!("preopen 插件配置目录失败（{e}），配置读写将不可用");
    }
    builder.build()
}

/// 插件配置目录：~/.tiangong/plugins/<plugin_id>/
fn plugin_config_dir(plugin_id: &str) -> std::path::PathBuf {
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
        .join(plugin_id)
}
