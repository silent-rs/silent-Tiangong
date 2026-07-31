//! 已安装 WASM 插件的发现、加载和全局页面注册。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use tiangong_core::core::Plugin;

use crate::adapter::{WasmPluginAdapter, call_wasm_off_runtime};
use crate::config::PluginRuntimeConfig;
use crate::loader::{Contribution, WasmPlugin, WasmPluginLoader};
use crate::manifest::{MANIFEST_FILE, PluginManifest};
use crate::sidecar::{ProcessSidecarConnection, SidecarConfig, SidecarConnection};

static LOADED_PLUGINS: OnceLock<Mutex<HashMap<String, Arc<Mutex<WasmPlugin>>>>> = OnceLock::new();
static SIDECAR_CONNECTIONS: OnceLock<Mutex<HashMap<PathBuf, Arc<ProcessSidecarConnection>>>> =
    OnceLock::new();

struct InstalledPlugin {
    directory: PathBuf,
    manifest: PluginManifest,
}

fn loaded_plugins() -> &'static Mutex<HashMap<String, Arc<Mutex<WasmPlugin>>>> {
    LOADED_PLUGINS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sidecar_connections() -> &'static Mutex<HashMap<PathBuf, Arc<ProcessSidecarConnection>>> {
    SIDECAR_CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_plugin(id: String, plugin: Arc<Mutex<WasmPlugin>>) {
    if let Ok(mut table) = loaded_plugins().lock() {
        tracing::info!(plugin_id = %id, "WASM 插件已注册");
        table.insert(id, plugin);
    }
}

/// 发现并加载 `storage_root/plugins/*/plugin.json` 中声明的所有插件。
pub fn load_installed_plugins(storage_root: &Path) -> Vec<Arc<dyn Plugin>> {
    discover_installed_plugins(storage_root)
        .into_iter()
        .filter_map(|installed| load_installed_plugin(storage_root, installed))
        .collect()
}

/// 通过插件 ID 调用其 sidecar，入口不需要了解制品位置或传输协议。
pub fn invoke_sidecar(
    storage_root: &Path,
    plugin_id: &str,
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let installed = find_installed_plugin(storage_root, plugin_id)?;
    let connection = sidecar_connection(storage_root, &installed)?;
    let payload = serde_json::to_string(&payload).with_context(|| "序列化插件请求失败")?;
    let response = connection.invoke(operation, &payload)?;
    serde_json::from_str(&response).with_context(|| "解析插件响应失败")
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

/// 处理插件页面消息（iframe 与插件双向通信）。
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

/// 从指定路径加载 WASM 插件，供运行时集成测试使用。
pub fn load_wasm_plugin_at(
    wasm_path: &Path,
    sidecar: Option<Arc<dyn SidecarConnection>>,
) -> Option<Arc<dyn Plugin>> {
    let plugin_id = wasm_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("plugin")
        .to_string();
    load_wasm_plugin_at_for_id(wasm_path, &plugin_id, sidecar, false)
}

fn load_installed_plugin(
    storage_root: &Path,
    installed: InstalledPlugin,
) -> Option<Arc<dyn Plugin>> {
    let wasm_path = installed.directory.join(&installed.manifest.wasm);
    if !wasm_path.is_file() {
        tracing::warn!(
            plugin_id = %installed.manifest.id,
            path = %wasm_path.display(),
            "插件 WASM 制品不存在"
        );
        return None;
    }

    let sidecar = installed.manifest.sidecar.as_ref().and_then(|_| {
        match sidecar_connection(storage_root, &installed) {
            Ok(connection) => {
                if let Err(error) = connection.ensure_running() {
                    tracing::warn!(
                        plugin_id = %installed.manifest.id,
                        %error,
                        "插件 sidecar 暂不可用，后续调用时将重试"
                    );
                }
                Some(connection as Arc<dyn SidecarConnection>)
            }
            Err(error) => {
                tracing::warn!(plugin_id = %installed.manifest.id, %error, "创建 sidecar 连接失败");
                None
            }
        }
    });

    load_wasm_plugin_at_for_id(&wasm_path, &installed.manifest.id, sidecar, true)
}

fn load_wasm_plugin_at_for_id(
    wasm_path: &Path,
    plugin_id: &str,
    sidecar: Option<Arc<dyn SidecarConnection>>,
    validate_id: bool,
) -> Option<Arc<dyn Plugin>> {
    let wasm_path = wasm_path.to_path_buf();
    let plugin_id = plugin_id.to_string();
    match crate::execution::run_outside_tokio(move || {
        load_wasm_plugin_at_inner(&wasm_path, &plugin_id, sidecar, validate_id)
            .ok_or_else(|| anyhow::anyhow!("WASM 插件加载失败"))
    }) {
        Ok(plugin) => Some(plugin),
        Err(error) => {
            tracing::warn!(%error, "加载 WASM 插件失败");
            None
        }
    }
}

fn load_wasm_plugin_at_inner(
    wasm_path: &Path,
    plugin_id: &str,
    sidecar: Option<Arc<dyn SidecarConnection>>,
    validate_id: bool,
) -> Option<Arc<dyn Plugin>> {
    let config = PluginRuntimeConfig::default();
    let loader = WasmPluginLoader::with_sidecar(&config, sidecar)
        .map_err(|error| {
            tracing::warn!(%error, "创建 WASM 加载器失败");
        })
        .ok()?;
    let plugin = loader
        .load_for_plugin(wasm_path, &config, plugin_id)
        .map_err(|error| tracing::warn!(%error, "实例化 WASM 插件失败"))
        .ok()?;
    let adapter = WasmPluginAdapter::new(plugin, config);
    if validate_id && adapter.id() != plugin_id {
        tracing::warn!(
            manifest_id = %plugin_id,
            component_id = %adapter.id(),
            "插件清单 ID 与组件描述不一致"
        );
        return None;
    }
    register_plugin(adapter.id().to_string(), adapter.inner_handle());
    Some(Arc::new(adapter) as Arc<dyn Plugin>)
}

fn discover_installed_plugins(storage_root: &Path) -> Vec<InstalledPlugin> {
    let plugins_dir = storage_root.join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins_dir) else {
        return Vec::new();
    };
    let mut manifest_paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(MANIFEST_FILE))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifest_paths.sort();

    manifest_paths
        .into_iter()
        .filter_map(|path| match PluginManifest::load(&path) {
            Ok(manifest) => Some(InstalledPlugin {
                directory: path.parent()?.to_path_buf(),
                manifest,
            }),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "忽略无效插件清单");
                None
            }
        })
        .collect()
}

fn find_installed_plugin(storage_root: &Path, plugin_id: &str) -> Result<InstalledPlugin> {
    discover_installed_plugins(storage_root)
        .into_iter()
        .find(|installed| installed.manifest.id == plugin_id)
        .ok_or_else(|| anyhow::anyhow!("插件未安装: {plugin_id}"))
}

fn sidecar_connection(
    storage_root: &Path,
    installed: &InstalledPlugin,
) -> Result<Arc<ProcessSidecarConnection>> {
    let sidecar = installed
        .manifest
        .sidecar
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("插件 {} 未声明 sidecar", installed.manifest.id))?;
    let mut binary = installed.directory.join(&sidecar.binary);
    if !std::env::consts::EXE_SUFFIX.is_empty() {
        let file_name = binary
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("sidecar 文件名无效"))?;
        if !file_name.ends_with(std::env::consts::EXE_SUFFIX) {
            binary.set_file_name(format!("{file_name}{}", std::env::consts::EXE_SUFFIX));
        }
    }
    let endpoint = storage_root.join(&sidecar.endpoint);
    let log = storage_root.join(&sidecar.log);

    let connection = {
        let mut connections = sidecar_connections()
            .lock()
            .map_err(|_| anyhow::anyhow!("插件 sidecar 连接表已损坏"))?;
        connections
            .entry(binary.clone())
            .or_insert_with(|| {
                Arc::new(ProcessSidecarConnection::new(SidecarConfig::new(
                    &installed.manifest.id,
                    binary,
                    endpoint,
                    log,
                )))
            })
            .clone()
    };
    Ok(connection)
}
