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
use crate::bindings::tiangong::plugin::feedback::Host as FeedbackHost;
use crate::bindings::tiangong::plugin::sidecar::{Host as SidecarHost, SidecarError};
use crate::sidecar::{SidecarConnection, SidecarInvokeError};
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_types::StreamEvent;

/// WASM Store 的宿主侧状态。
pub struct HostState {
    limits: StoreLimits,
    wasi: WasiCtx,
    table: ResourceTable,
    /// sidecar 连接（通用，由入口侧注入）。None 时 invoke 返回 unavailable。
    sidecar: Option<Arc<dyn SidecarConnection>>,
    /// 当前 turn 的插件反馈通道。
    feedback: Option<PluginFeedbackTx>,
    /// 插件 ID（构造时用于 preopen 插件配置目录）。
    #[allow(dead_code)]
    plugin_id: String,
}

impl HostState {
    pub fn new(
        limits: StoreLimits,
        sidecar: Option<Arc<dyn SidecarConnection>>,
        plugin_id: String,
        storage_access: bool,
    ) -> Self {
        let wasi = build_wasi_ctx(&plugin_id, storage_access);
        Self {
            limits,
            wasi,
            table: ResourceTable::new(),
            sidecar,
            feedback: None,
            plugin_id,
        }
    }

    /// 提供对内部限制器的可变借用，供 `Store::limiter` 闭包返回。
    pub fn limits_mut(&mut self) -> &mut StoreLimits {
        &mut self.limits
    }

    pub fn set_feedback(&mut self, feedback: PluginFeedbackTx) {
        self.feedback = Some(feedback);
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

impl FeedbackHost for HostState {
    fn emit_stream_event(&mut self, event_json: String) {
        let Some(feedback) = &self.feedback else {
            return;
        };
        match serde_json::from_str::<StreamEvent>(&event_json) {
            Ok(event) => feedback.send_stream_event(event),
            Err(error) => tracing::warn!(%error, "wasm 插件反馈事件解析失败"),
        }
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
        let feedback = self.feedback.clone();
        std::thread::scope(|s| {
            s.spawn(move || {
                let mut on_progress = |event_json: String| {
                    let Some(feedback) = &feedback else {
                        return;
                    };
                    match serde_json::from_str::<StreamEvent>(&event_json) {
                        Ok(event) => feedback.send_stream_event(event),
                        Err(error) => tracing::warn!(%error, "sidecar 进度事件解析失败"),
                    }
                };
                conn.invoke_with_progress(&operation, &payload, &mut on_progress)
            })
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
///
/// `storage_access` 为 true 时额外 preopen 天工存储根目录（~/.tiangong），
/// 映射为 WASM 内的 `/storage`，供需要访问全局配置文件（如 custom-prompt.md）的插件使用。
fn build_wasi_ctx(plugin_id: &str, storage_access: bool) -> WasiCtx {
    let mut builder = WasiCtxBuilder::new();
    let dir = plugin_config_dir(plugin_id);
    let _ = std::fs::create_dir_all(&dir);
    if let Err(e) = builder.preopened_dir(&dir, ".", wasmtime_wasi::FsPerms::ReadWrite) {
        tracing::debug!("preopen 插件配置目录失败（{e}），配置读写将不可用");
    }
    if storage_access {
        let storage_root = plugin_storage_root();
        if let Some(root) = &storage_root {
            let _ = std::fs::create_dir_all(root);
            if let Err(e) =
                builder.preopened_dir(root, "/storage", wasmtime_wasi::FsPerms::ReadWrite)
            {
                tracing::debug!("preopen 存储根目录失败（{e}），存储根访问将不可用");
            }
        }
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

/// 天工存储根目录：~/.tiangong/
fn plugin_storage_root() -> Option<std::path::PathBuf> {
    fn user_home() -> Option<std::path::PathBuf> {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            return Some(std::path::PathBuf::from(home));
        }
        std::env::var_os("USERPROFILE")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
    }
    user_home().map(|h| h.join(".tiangong"))
}
