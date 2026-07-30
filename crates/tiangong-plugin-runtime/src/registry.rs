//! WASM 插件运行时注册便捷函数。
//!
//! 供三入口（CLI/Server/Desktop）在插件拼装时加载单文件 WASM memory 插件。
//! 加载失败（文件缺失、实例化错误）时优雅降级返回 None，不影响原生插件。

use std::path::Path;
use std::sync::Arc;

use tiangong_core::core::Plugin;
use tiangong_memory::MemoryHandle;

use crate::adapter::WasmPluginAdapter;
use crate::config::PluginRuntimeConfig;
use crate::loader::WasmPluginLoader;

/// memory wasm 组件的固定文件名（由 xtask build-wasm 部署）。
const MEMORY_WASM_FILE: &str = "tiangong_plugin_memory_wasm.wasm";

/// 从 `storage_root/plugins/` 加载 memory wasm 插件。
///
/// - 文件不存在 → 返回 None（优雅降级，仅用原生 memory 插件）
/// - 加载/实例化失败 → 记录 warning 并返回 None
/// - 成功 → 返回包装为 `Arc<dyn Plugin>` 的 [`WasmPluginAdapter`]
///
/// `memory_handle` 注入给 memory-store host import，用于查询真实记忆；
/// 为 None 时 wasm 内 recall 回退到 mock。
pub fn load_memory_wasm_plugin(
    storage_root: &Path,
    memory_handle: Option<MemoryHandle>,
) -> Option<Arc<dyn Plugin>> {
    let wasm_path = storage_root.join("plugins").join(MEMORY_WASM_FILE);
    if !wasm_path.exists() {
        tracing::debug!(
            "memory wasm 插件不存在（{}），跳过 wasm 加载，使用原生 memory 插件",
            wasm_path.display()
        );
        return None;
    }
    load_wasm_plugin_at(&wasm_path, memory_handle)
}

/// 从指定路径加载 wasm 插件（测试用）。
pub fn load_wasm_plugin_at(
    wasm_path: &Path,
    memory_handle: Option<MemoryHandle>,
) -> Option<Arc<dyn Plugin>> {
    let config = PluginRuntimeConfig::default();
    let loader = match WasmPluginLoader::with_memory(&config, memory_handle) {
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
    Some(Arc::new(WasmPluginAdapter::new(plugin, config)) as Arc<dyn Plugin>)
}
