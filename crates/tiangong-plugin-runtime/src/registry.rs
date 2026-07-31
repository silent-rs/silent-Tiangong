//! WASM 插件运行时注册便捷函数。
//!
//! 供三入口（CLI/Server/Desktop）在插件拼装时加载单文件 WASM memory 插件。
//! 加载失败（文件缺失、实例化错误）时优雅降级返回 None，不影响原生插件。
//!
//! 加载成功的插件同时注册到全局表，供 Tauri 命令查询 contributions / config。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use tiangong_core::core::Plugin;

use crate::adapter::{WasmPluginAdapter, call_wasm_off_runtime};
use crate::config::PluginRuntimeConfig;
use crate::loader::{Contribution, WasmPlugin, WasmPluginLoader};
use crate::sidecar::SidecarConnection;

/// memory wasm 组件的固定文件名（由 xtask build-wasm 部署）。
const MEMORY_WASM_FILE: &str = "tiangong_plugin_memory_wasm.wasm";

/// 全局已加载的 WASM 插件注册表（plugin_id → WasmPlugin 句柄）。
/// 供 Tauri 命令查询 contributions / config。
static LOADED_PLUGINS: OnceLock<Mutex<HashMap<String, Arc<Mutex<WasmPlugin>>>>> = OnceLock::new();

fn loaded_plugins() -> &'static Mutex<HashMap<String, Arc<Mutex<WasmPlugin>>>> {
    LOADED_PLUGINS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 注册一个已加载的 WASM 插件到全局表。
fn register_plugin(id: String, plugin: Arc<Mutex<WasmPlugin>>) {
    if let Ok(mut table) = loaded_plugins().lock() {
        tracing::info!("WASM 插件已注册: {id}");
        table.insert(id, plugin);
    }
}

/// 收集所有已加载 WASM 插件的设置页贡献。
pub fn list_contributions() -> Vec<(String, Vec<Contribution>)> {
    let entries = {
        let Ok(table) = loaded_plugins().lock() else {
            return Vec::new();
        };
        table
            .iter()
            .map(|(id, plugin)| (id.clone(), plugin.clone()))
            .collect::<Vec<_>>()
    };
    entries
        .iter()
        .filter_map(|(id, plugin)| {
            call_wasm_off_runtime(plugin.clone(), WasmPlugin::contributions)
                .ok()
                .map(|contributions| (id.clone(), contributions))
        })
        .collect()
}

/// 打开插件页面，返回入口 HTML。
pub fn open_view(plugin_id: &str, contribution_id: &str) -> Option<String> {
    let table = loaded_plugins().lock().ok()?;
    let plugin = table.get(plugin_id)?.clone();
    drop(table);
    let contribution_id = contribution_id.to_string();
    call_wasm_off_runtime(plugin, move |plugin| plugin.open_view(contribution_id)).ok()
}

/// 获取插件页面资源（字节 + MIME）。
pub fn get_view_resource(plugin_id: &str, path: &str) -> Option<(Vec<u8>, String)> {
    let table = loaded_plugins().lock().ok()?;
    let plugin = table.get(plugin_id)?.clone();
    drop(table);
    let path = path.to_string();
    call_wasm_off_runtime(plugin, move |plugin| plugin.get_view_resource(path)).ok()
}

/// 处理插件页面消息（iframe ↔ 插件双向通信）。
pub fn handle_view_message(plugin_id: &str, method: &str, payload: &str) -> Option<String> {
    let table = loaded_plugins().lock().ok()?;
    let plugin = table.get(plugin_id)?.clone();
    drop(table);
    let method = method.to_string();
    let payload = payload.to_string();
    call_wasm_off_runtime(plugin, move |plugin| {
        plugin.handle_view_message(method, payload)
    })
    .ok()
}

/// 从 `storage_root/plugins/` 加载 memory wasm 插件。
///
/// `sidecar` 注入给 sidecar host import，用于转发请求到配套 sidecar；
/// 为 None 时 invoke 返回 unavailable。
pub fn load_memory_wasm_plugin(
    storage_root: &Path,
    sidecar: Option<Arc<dyn SidecarConnection>>,
) -> Option<Arc<dyn Plugin>> {
    let wasm_path = storage_root.join("plugins").join(MEMORY_WASM_FILE);
    if !wasm_path.exists() {
        tracing::info!("memory wasm 插件不存在（{}），跳过", wasm_path.display());
        return None;
    }
    load_wasm_plugin_at(&wasm_path, sidecar)
}

/// 从指定路径加载 wasm 插件（测试用）。
pub fn load_wasm_plugin_at(
    wasm_path: &Path,
    sidecar: Option<Arc<dyn SidecarConnection>>,
) -> Option<Arc<dyn Plugin>> {
    let wasm_path = wasm_path.to_path_buf();
    match crate::execution::run_outside_tokio(move || {
        load_wasm_plugin_at_inner(&wasm_path, sidecar)
            .ok_or_else(|| anyhow::anyhow!("WASM 插件加载失败"))
    }) {
        Ok(plugin) => Some(plugin),
        Err(e) => {
            tracing::warn!("加载 wasm 插件失败: {e}");
            None
        }
    }
}

fn load_wasm_plugin_at_inner(
    wasm_path: &Path,
    sidecar: Option<Arc<dyn SidecarConnection>>,
) -> Option<Arc<dyn Plugin>> {
    let config = PluginRuntimeConfig::default();
    let loader = match WasmPluginLoader::with_sidecar(&config, sidecar) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("创建 wasm 加载器失败: {e}");
            return None;
        }
    };
    let plugin = match loader.load(wasm_path, &config) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("加载 wasm memory 插件失败: {e}");
            return None;
        }
    };
    let adapter = WasmPluginAdapter::new(plugin, config);
    // 注册到全局表，供 Tauri 命令查询 contributions/config。
    register_plugin(adapter.id().to_string(), adapter.inner_handle());
    Some(Arc::new(adapter) as Arc<dyn Plugin>)
}
