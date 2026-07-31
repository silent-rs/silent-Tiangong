//! 已安装 WASM 插件的发现、加载、状态查询和动态热加载。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tiangong_core::core::Plugin;

use crate::adapter::{WasmPluginAdapter, call_wasm_off_runtime};
use crate::config::PluginRuntimeConfig;
use crate::loader::{Contribution, Descriptor, WasmPlugin, WasmPluginLoader};
use crate::manifest::{MANIFEST_FILE, PluginManifest};
use crate::sidecar::{ProcessSidecarConnection, SidecarConfig, SidecarConnection};

static LOADED_PLUGINS: OnceLock<Mutex<HashMap<String, LoadedPlugin>>> = OnceLock::new();
static SIDECAR_CONNECTIONS: OnceLock<Mutex<HashMap<PathBuf, Arc<ProcessSidecarConnection>>>> =
    OnceLock::new();
static LOAD_OPERATION: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct InstalledPlugin {
    directory: PathBuf,
    manifest: PluginManifest,
}

struct LoadedPlugin {
    directory: PathBuf,
    manifest: PluginManifest,
    wasm_bytes: Option<Arc<Vec<u8>>>,
    ui_plugin: Option<Arc<Mutex<WasmPlugin>>>,
    descriptor: Option<Descriptor>,
    generation: u64,
    instances: Vec<Weak<WasmPluginAdapter>>,
    sidecar: Option<Arc<ProcessSidecarConnection>>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatus {
    pub id: String,
    pub name: String,
    pub manifest_version: String,
    pub loaded_version: Option<String>,
    pub state: String,
    pub generation: u64,
    pub has_sidecar: bool,
    pub sidecar_running: bool,
    pub last_error: Option<String>,
}

fn loaded_plugins() -> &'static Mutex<HashMap<String, LoadedPlugin>> {
    LOADED_PLUGINS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sidecar_connections() -> &'static Mutex<HashMap<PathBuf, Arc<ProcessSidecarConnection>>> {
    SIDECAR_CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 预加载设置页实例，供尚未创建 Core 时查询插件贡献。
pub fn preload_installed_plugins(storage_root: &Path) -> usize {
    let Ok(_operation) = LOAD_OPERATION.lock() else {
        tracing::warn!("插件加载操作锁已损坏");
        return 0;
    };

    let installed_plugins = discover_installed_plugins(storage_root);
    for installed in &installed_plugins {
        let exists = loaded_plugins()
            .lock()
            .map(|plugins| plugins.contains_key(&installed.manifest.id))
            .unwrap_or(false);
        if exists {
            continue;
        }

        let loaded = load_plugin_record(storage_root, installed.clone());
        if let Ok(mut plugins) = loaded_plugins().lock() {
            plugins.insert(installed.manifest.id.clone(), loaded);
        }
    }

    installed_plugins.len()
}

/// 为一个 Core 创建独立实例。实例使用注册表已接受的同一份 WASM 字节快照。
pub fn load_installed_plugins(storage_root: &Path) -> Vec<Arc<dyn Plugin>> {
    preload_installed_plugins(storage_root);
    let Ok(_operation) = LOAD_OPERATION.lock() else {
        tracing::warn!("插件加载操作锁已损坏");
        return Vec::new();
    };

    let plugin_ids = discover_installed_plugins(storage_root)
        .into_iter()
        .map(|installed| installed.manifest.id)
        .collect::<Vec<_>>();

    plugin_ids
        .into_iter()
        .filter_map(|plugin_id| load_core_plugin(&plugin_id))
        .collect()
}

/// 返回已安装插件状态，并探测 sidecar 当前是否可用。
pub fn list_plugins(storage_root: &Path) -> Vec<PluginStatus> {
    preload_installed_plugins(storage_root);
    let installed = discover_installed_plugins(storage_root);
    let snapshots = {
        let Ok(plugins) = loaded_plugins().lock() else {
            return Vec::new();
        };
        installed
            .into_iter()
            .map(|item| {
                let status = plugins.get(&item.manifest.id).map(|loaded| {
                    (
                        loaded.descriptor.clone(),
                        loaded.generation,
                        loaded.sidecar.clone(),
                        loaded.last_error.clone(),
                        loaded.ui_plugin.is_some(),
                    )
                });
                (item.manifest, status)
            })
            .collect::<Vec<_>>()
    };

    let mut statuses = snapshots
        .into_iter()
        .map(|(manifest, loaded)| {
            let (descriptor, generation, sidecar, last_error, has_ui) = loaded.unwrap_or_default();
            let sidecar_running = sidecar
                .as_ref()
                .is_some_and(|connection| connection.is_running());
            let state = if !has_ui {
                "error"
            } else if last_error.is_some() || (manifest.sidecar.is_some() && !sidecar_running) {
                "degraded"
            } else {
                "loaded"
            };
            PluginStatus {
                id: manifest.id.clone(),
                name: descriptor
                    .as_ref()
                    .map(|value| value.name.clone())
                    .unwrap_or_else(|| manifest.id.clone()),
                manifest_version: manifest.version,
                loaded_version: descriptor.map(|value| value.version),
                state: state.to_string(),
                generation,
                has_sidecar: manifest.sidecar.is_some(),
                sidecar_running,
                last_error,
            }
        })
        .collect::<Vec<_>>();
    statuses.sort_by(|left, right| left.id.cmp(&right.id));
    statuses
}

/// 从磁盘读取插件新版本。全部 UI/Core 实例成功创建后才切换。
pub fn reload_plugin(storage_root: &Path, plugin_id: &str) -> Result<PluginStatus> {
    let _operation = LOAD_OPERATION
        .lock()
        .map_err(|_| anyhow::anyhow!("插件加载操作锁已损坏"))?;
    let installed = find_installed_plugin(storage_root, plugin_id)?;

    let result = reload_plugin_inner(storage_root, &installed);
    if let Err(error) = &result {
        set_last_error(plugin_id, error.to_string());
    }
    result?;

    list_plugin_status_without_preload(&installed.manifest)
        .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 热加载后状态丢失"))
}

fn reload_plugin_inner(storage_root: &Path, installed: &InstalledPlugin) -> Result<()> {
    let wasm_bytes = Arc::new(read_wasm_bytes(installed)?);
    let sidecar = resolve_sidecar(storage_root, installed, true)?;
    if let Some(connection) = &sidecar {
        connection.ensure_running()?;
    }

    let (instances, next_generation) = {
        let plugins = loaded_plugins()
            .lock()
            .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?;
        let loaded = plugins
            .get(&installed.manifest.id)
            .ok_or_else(|| anyhow::anyhow!("插件 {} 尚未预加载", installed.manifest.id))?;
        (
            loaded
                .instances
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>(),
            loaded.generation.saturating_add(1).max(1),
        )
    };

    let runtime_config = PluginRuntimeConfig::default();
    let (ui_plugin, descriptor) = instantiate_snapshot(
        wasm_bytes.clone(),
        &installed.manifest,
        sidecar.clone(),
        runtime_config,
    )?;

    let mut replacements = Vec::with_capacity(instances.len());
    for adapter in &instances {
        let (plugin, _) = instantiate_snapshot(
            wasm_bytes.clone(),
            &installed.manifest,
            sidecar.clone(),
            adapter.runtime_config(),
        )?;
        let adapter = adapter.clone();
        let replacement =
            crate::execution::run_outside_tokio(move || adapter.prepare_replacement(plugin))?;
        replacements.push(replacement);
    }

    for (adapter, replacement) in instances.iter().zip(replacements) {
        adapter.replace_inner(replacement);
    }

    let mut plugins = loaded_plugins()
        .lock()
        .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?;
    let loaded = plugins
        .get_mut(&installed.manifest.id)
        .ok_or_else(|| anyhow::anyhow!("插件 {} 在切换前被移除", installed.manifest.id))?;
    loaded.directory = installed.directory.clone();
    loaded.manifest = installed.manifest.clone();
    loaded.wasm_bytes = Some(wasm_bytes);
    loaded.ui_plugin = Some(Arc::new(Mutex::new(ui_plugin)));
    loaded.descriptor = Some(descriptor);
    loaded.generation = next_generation;
    loaded.instances = instances.iter().map(Arc::downgrade).collect();
    loaded.sidecar = sidecar;
    loaded.last_error = None;
    tracing::info!(
        plugin_id = %installed.manifest.id,
        generation = next_generation,
        instances = instances.len(),
        "WASM 插件热加载完成"
    );
    Ok(())
}

/// 通过插件 ID 调用其 sidecar，入口不需要了解制品位置或传输协议。
pub fn invoke_sidecar(
    storage_root: &Path,
    plugin_id: &str,
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let installed = find_installed_plugin(storage_root, plugin_id)?;
    let connection = sidecar_connection(storage_root, &installed, false)?;
    let payload = serde_json::to_string(&payload).with_context(|| "序列化插件请求失败")?;
    let response = connection.invoke(operation, &payload)?;
    serde_json::from_str(&response).with_context(|| "解析插件响应失败")
}

/// 收集所有已加载 WASM 插件的设置页贡献及其加载代次。
pub fn list_contributions() -> Vec<(String, u64, Vec<Contribution>)> {
    let entries = {
        let Ok(plugins) = loaded_plugins().lock() else {
            return Vec::new();
        };
        plugins
            .iter()
            .filter_map(|(id, loaded)| {
                loaded
                    .ui_plugin
                    .as_ref()
                    .map(|plugin| (id.clone(), loaded.generation, plugin.clone()))
            })
            .collect::<Vec<_>>()
    };
    entries
        .into_iter()
        .filter_map(|(id, generation, plugin)| {
            call_wasm_off_runtime(plugin, WasmPlugin::contributions)
                .ok()
                .map(|contributions| (id, generation, contributions))
        })
        .collect()
}

/// 打开插件页面，返回入口 HTML。
pub fn open_view(plugin_id: &str, contribution_id: &str) -> Option<String> {
    let plugin = ui_plugin(plugin_id)?;
    let contribution_id = contribution_id.to_string();
    call_wasm_off_runtime(plugin, move |plugin| plugin.open_view(contribution_id)).ok()
}

/// 获取插件页面资源（字节 + MIME）。
pub fn get_view_resource(plugin_id: &str, path: &str) -> Option<(Vec<u8>, String)> {
    let plugin = ui_plugin(plugin_id)?;
    let path = path.to_string();
    call_wasm_off_runtime(plugin, move |plugin| plugin.get_view_resource(path)).ok()
}

/// 处理插件页面消息（iframe 与插件双向通信）。
pub fn handle_view_message(plugin_id: &str, method: &str, payload: &str) -> Option<String> {
    let plugin = ui_plugin(plugin_id)?;
    let method = method.to_string();
    let payload = payload.to_string();
    call_wasm_off_runtime(plugin, move |plugin| {
        plugin.handle_view_message(method, payload)
    })
    .ok()
}

fn ui_plugin(plugin_id: &str) -> Option<Arc<Mutex<WasmPlugin>>> {
    loaded_plugins()
        .lock()
        .ok()?
        .get(plugin_id)?
        .ui_plugin
        .clone()
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
    let bytes = Arc::new(std::fs::read(wasm_path).ok()?);
    let config = PluginRuntimeConfig::default();
    let plugin_id_for_load = plugin_id.clone();
    let plugin = crate::execution::run_outside_tokio(move || {
        let loader = WasmPluginLoader::with_sidecar(&config, sidecar)?;
        loader.load_bytes_for_plugin(&bytes, &config, &plugin_id_for_load)
    })
    .ok()?;
    Some(Arc::new(WasmPluginAdapter::new(
        plugin,
        PluginRuntimeConfig::default(),
    )))
}

fn load_plugin_record(storage_root: &Path, installed: InstalledPlugin) -> LoadedPlugin {
    let sidecar_result = resolve_sidecar(storage_root, &installed, false);
    let (sidecar, mut last_error) = match sidecar_result {
        Ok(connection) => (connection, None),
        Err(error) => (None, Some(error.to_string())),
    };
    if let Some(connection) = &sidecar {
        if let Err(error) = connection.ensure_running() {
            tracing::warn!(plugin_id = %installed.manifest.id, %error, "插件 sidecar 暂不可用");
            last_error = Some(error.to_string());
        }
    }

    let load_result = read_wasm_bytes(&installed).and_then(|bytes| {
        let bytes = Arc::new(bytes);
        instantiate_snapshot(
            bytes.clone(),
            &installed.manifest,
            sidecar.clone(),
            PluginRuntimeConfig::default(),
        )
        .map(|(plugin, descriptor)| (bytes, plugin, descriptor))
    });

    match load_result {
        Ok((bytes, plugin, descriptor)) => {
            tracing::info!(plugin_id = %installed.manifest.id, "WASM 插件已预加载");
            LoadedPlugin {
                directory: installed.directory,
                manifest: installed.manifest,
                wasm_bytes: Some(bytes),
                ui_plugin: Some(Arc::new(Mutex::new(plugin))),
                descriptor: Some(descriptor),
                generation: 1,
                instances: Vec::new(),
                sidecar,
                last_error,
            }
        }
        Err(error) => {
            tracing::warn!(plugin_id = %installed.manifest.id, %error, "加载 WASM 插件失败");
            LoadedPlugin {
                directory: installed.directory,
                manifest: installed.manifest,
                wasm_bytes: None,
                ui_plugin: None,
                descriptor: None,
                generation: 0,
                instances: Vec::new(),
                sidecar,
                last_error: Some(error.to_string()),
            }
        }
    }
}

fn load_core_plugin(plugin_id: &str) -> Option<Arc<dyn Plugin>> {
    let (manifest, bytes, sidecar) = {
        let plugins = loaded_plugins().lock().ok()?;
        let loaded = plugins.get(plugin_id)?;
        (
            loaded.manifest.clone(),
            loaded.wasm_bytes.clone()?,
            loaded.sidecar.clone(),
        )
    };
    let (plugin, _) =
        match instantiate_snapshot(bytes, &manifest, sidecar, PluginRuntimeConfig::default()) {
            Ok(value) => value,
            Err(error) => {
                set_last_error(plugin_id, error.to_string());
                tracing::warn!(plugin_id, %error, "创建 Core WASM 插件实例失败");
                return None;
            }
        };
    let adapter = Arc::new(WasmPluginAdapter::new(
        plugin,
        PluginRuntimeConfig::default(),
    ));
    if let Ok(mut plugins) = loaded_plugins().lock()
        && let Some(loaded) = plugins.get_mut(plugin_id)
    {
        loaded
            .instances
            .retain(|instance| instance.strong_count() > 0);
        loaded.instances.push(Arc::downgrade(&adapter));
    }
    Some(adapter)
}

fn instantiate_snapshot(
    bytes: Arc<Vec<u8>>,
    manifest: &PluginManifest,
    sidecar: Option<Arc<ProcessSidecarConnection>>,
    config: PluginRuntimeConfig,
) -> Result<(WasmPlugin, Descriptor)> {
    let plugin_id = manifest.id.clone();
    let expected_version = manifest.version.clone();
    crate::execution::run_outside_tokio(move || {
        let sidecar = sidecar.map(|value| value as Arc<dyn SidecarConnection>);
        let loader = WasmPluginLoader::with_sidecar(&config, sidecar)?;
        let mut plugin = loader.load_bytes_for_plugin(&bytes, &config, &plugin_id)?;
        let descriptor = plugin.describe()?;
        if descriptor.id != plugin_id {
            bail!(
                "插件清单 ID 与组件描述不一致: manifest={plugin_id}, component={}",
                descriptor.id
            );
        }
        if descriptor.version != expected_version {
            bail!(
                "插件清单版本与组件描述不一致: manifest={expected_version}, component={}",
                descriptor.version
            );
        }
        Ok((plugin, descriptor))
    })
}

fn read_wasm_bytes(installed: &InstalledPlugin) -> Result<Vec<u8>> {
    let path = installed.directory.join(installed.manifest.wasm_binary());
    std::fs::read(&path).with_context(|| format!("读取插件 WASM 制品失败: {}", path.display()))
}

fn set_last_error(plugin_id: &str, error: String) {
    if let Ok(mut plugins) = loaded_plugins().lock()
        && let Some(plugin) = plugins.get_mut(plugin_id)
    {
        plugin.last_error = Some(error);
    }
}

fn list_plugin_status_without_preload(manifest: &PluginManifest) -> Option<PluginStatus> {
    let (descriptor, generation, sidecar, last_error, has_ui) = {
        let plugins = loaded_plugins().lock().ok()?;
        let loaded = plugins.get(&manifest.id)?;
        (
            loaded.descriptor.clone(),
            loaded.generation,
            loaded.sidecar.clone(),
            loaded.last_error.clone(),
            loaded.ui_plugin.is_some(),
        )
    };
    let sidecar_running = sidecar
        .as_ref()
        .is_some_and(|connection| connection.is_running());
    let state = if !has_ui {
        "error"
    } else if last_error.is_some() || (manifest.sidecar.is_some() && !sidecar_running) {
        "degraded"
    } else {
        "loaded"
    };
    Some(PluginStatus {
        id: manifest.id.clone(),
        name: descriptor
            .as_ref()
            .map(|value| value.name.clone())
            .unwrap_or_else(|| manifest.id.clone()),
        manifest_version: manifest.version.clone(),
        loaded_version: descriptor.map(|value| value.version),
        state: state.to_string(),
        generation,
        has_sidecar: manifest.sidecar.is_some(),
        sidecar_running,
        last_error,
    })
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

fn resolve_sidecar(
    storage_root: &Path,
    installed: &InstalledPlugin,
    refresh: bool,
) -> Result<Option<Arc<ProcessSidecarConnection>>> {
    if installed.manifest.sidecar.is_none() {
        return Ok(None);
    }
    sidecar_connection(storage_root, installed, refresh).map(Some)
}

fn sidecar_connection(
    _storage_root: &Path,
    installed: &InstalledPlugin,
    refresh: bool,
) -> Result<Arc<ProcessSidecarConnection>> {
    let sidecar = installed
        .manifest
        .sidecar
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("插件 {} 未声明 sidecar", installed.manifest.id))?;
    if !installed.manifest.permissions.is_empty()
        && !installed.manifest.has_permission("sidecar.invoke")
    {
        bail!("插件 {} 未声明 sidecar.invoke 权限", installed.manifest.id);
    }

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
    let endpoint = installed.directory.join("runtime").join("endpoint.json");
    let log = installed.directory.join("logs").join("sidecar.log");
    let data_dir = installed.directory.join("data");
    let config = SidecarConfig::new(
        &installed.manifest.id,
        &installed.manifest.version,
        binary,
        endpoint,
        log,
        data_dir,
    )
    .with_protocols(&sidecar.transport_protocol, sidecar.business_protocol)
    .with_timeouts(
        Duration::from_millis(sidecar.startup_timeout_ms),
        Duration::from_millis(sidecar.request_timeout_ms),
    );

    let mut connections = sidecar_connections()
        .lock()
        .map_err(|_| anyhow::anyhow!("插件 sidecar 连接表已损坏"))?;
    if refresh || !connections.contains_key(&installed.directory) {
        connections.insert(
            installed.directory.clone(),
            Arc::new(ProcessSidecarConnection::new(config)),
        );
    }
    connections
        .get(&installed.directory)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("创建插件 sidecar 连接失败"))
}
