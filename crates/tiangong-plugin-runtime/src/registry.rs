//! 已安装 WASM 插件的发现、加载、状态查询和动态热加载。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Serialize;
use tiangong_core::core::Plugin;

use crate::adapter::{WasmPluginAdapter, call_wasm_off_runtime};
use crate::config::PluginRuntimeConfig;
use crate::loader::{
    Contribution, Descriptor, WasmPlugin, WasmPluginLoader, compile_component,
    instantiate_component,
};
use crate::manifest::{MANIFEST_FILE, PluginManifest};
use crate::sidecar::{SidecarConfig, SidecarConnection, StdioSidecarConnection, TRANSPORT_STDIO};
use crate::signature::{SignedPluginRelease, verify_signed_release};
use crate::ts_plugin::TsPluginAdapter;

static LOADED_PLUGINS: OnceLock<Mutex<HashMap<String, LoadedPlugin>>> = OnceLock::new();
/// 扫描发现但被忽略的无效插件（签名无效、沙箱越权、清单损坏）。
/// 随 `preload_installed_plugins` 全量刷新，供插件管理列表展示和清理。
static INVALID_PLUGINS: OnceLock<Mutex<Vec<InvalidPluginEntry>>> = OnceLock::new();
static SIDECAR_CONNECTIONS: OnceLock<Mutex<HashMap<PathBuf, Arc<dyn SidecarConnection>>>> =
    OnceLock::new();
static LOAD_OPERATION: Mutex<()> = Mutex::new(());

/// 全局 server 连接信息（可覆盖更新），供需要回调 host 的 sidecar 使用。
///
/// Server 启停、端口或令牌变化时，入口层会调 [`set_server_endpoint`] 更新此值，
/// 并重启依赖 server 的 sidecar（如 scheduler）。
static SERVER_ENDPOINT: Mutex<Option<(String, Option<String>)>> = Mutex::new(None);

/// 设置或更新本机 server 的连接信息。
///
/// 与上一次值不同时，会重启所有依赖 server 回调的 sidecar（当前为 scheduler），
/// 让它们用新的地址/令牌重新连接。
pub fn set_server_endpoint(url: String, token: Option<String>) {
    let restart_needed = {
        let mut guard = SERVER_ENDPOINT.lock().expect("SERVER_ENDPOINT 锁损坏");
        let changed = guard
            .as_ref()
            .map(|(prev_url, prev_token)| {
                prev_url != &url || prev_token.as_deref() != token.as_deref()
            })
            .unwrap_or(true);
        *guard = Some((url, token));
        changed
    };
    if restart_needed {
        restart_server_dependent_sidecars();
    }
}

/// 取当前已设置的 server 连接信息（未设置返回 None）。
fn current_server_endpoint() -> Option<(String, Option<String>)> {
    SERVER_ENDPOINT.lock().ok().and_then(|guard| guard.clone())
}

/// 重启依赖 server 回调的 sidecar（当前为 scheduler）。
///
/// Server 地址/令牌变化后，旧 sidecar 进程持有的 env 已过期，必须重启才能拿到新值。
/// 停止后，下次 invoke 时运行时会自动用新配置重新拉起 sidecar。
fn restart_server_dependent_sidecars() {
    let Some(home) = user_home_dir() else {
        tracing::warn!("重启 server 依赖 sidecar 时无法确定 home 目录，跳过");
        return;
    };
    let storage_root = home.join(".tiangong");
    for plugin_id in SERVER_DEPENDENT_PLUGINS {
        if let Err(error) = stop_installed_sidecar(&storage_root, plugin_id) {
            tracing::warn!(plugin_id, %error, "重启 server 依赖 sidecar 时停止失败");
        }
    }
}

/// 跨平台获取用户 home 目录（与 sidecar 框架 `endpoint::home_dir` 同源）。
fn user_home_dir() -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = std::path::PathBuf::from(drive);
            buf.push(path);
            Some(buf)
        }
        _ => None,
    }
}

/// 停止指定插件的 sidecar 进程（停止连接 + 清除连接缓存）。
///
/// 停止后下次 invoke 会用最新配置（含新 env）重新拉起 sidecar。
fn stop_installed_sidecar(storage_root: &Path, plugin_id: &str) -> Result<()> {
    let installed = find_installed_plugin(storage_root, plugin_id)?;
    stop_loaded_sidecar(plugin_id)?;
    stop_connection_for_directory(&installed.directory)?;
    tracing::info!(plugin_id, "已停止 sidecar，下次调用将以新配置重启");
    Ok(())
}

/// 依赖 server 回调的插件 ID 列表。
///
/// 这些 sidecar 会在运行时经 HTTP 回调本机 server，server 连接信息变化时必须重启。
const SERVER_DEPENDENT_PLUGINS: &[&str] = &["scheduler"];
const DISABLED_MARKER: &str = ".disabled";
const ROLLBACK_DIR: &str = ".rollback";

/// 天工运行入口类型。
///
/// 插件清单可声明 `entrypoints` 限定适用入口；runtime 据此过滤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Desktop,
    Cli,
    Server,
}

impl RuntimeKind {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Cli => "cli",
            Self::Server => "server",
        }
    }
}

/// 获取当前已配置的模型能力列表（snake_case），供过滤判断。
fn configured_model_capabilities() -> Vec<String> {
    let models = tiangong_config::registry::models();
    tiangong_llm::ModelCapability::all()
        .iter()
        .filter(|cap| models.resolve_for_capability(**cap).is_some())
        .map(|cap| cap.key().to_string())
        .collect()
}

/// 判断插件是否可在当前入口注册为 Core 工具。
///
/// 返回 `Some(reason)` 表示不可注册（reason 为跳过原因，供日志/管理页展示）。
fn check_plugin_availability(
    manifest: &PluginManifest,
    runtime: RuntimeKind,
    configured: &[String],
) -> Option<String> {
    // 入口过滤
    if !manifest.available_at(runtime.key()) {
        return Some(format!("当前入口 {} 不在插件声明的入口列表", runtime.key()));
    }
    // 必需模型能力过滤
    let configured_strs: Vec<&str> = configured.iter().map(String::as_str).collect();
    let missing = manifest.missing_capabilities(&configured_strs);
    if !missing.is_empty() {
        return Some(format!("缺少必需模型能力：{}", missing.join(", ")));
    }
    None
}
const PRESERVED_ENTRIES: [&str; 3] = ["runtime", "logs", "data"];

/// 旧编号插件 ID → 统一命名后的新 ID。
///
/// 「统一官方插件命名」去掉官方插件 ID 的 `-handler` 后缀后，宿主与前端
/// 全部链路只认新编号；存量安装目录不会自动跟随（插件目录亦无对应制品
/// 可供升级检测），需在装载前迁移，否则旧插件注册的工具接到已退役的
/// 链路上，表现为工具可调用但界面不响应（如 web_fetch 不再弹出浏览器）。
const LEGACY_PLUGIN_ID_MAP: [(&str, &str); 3] = [
    ("browser-handler", "browser"),
    ("terminal-handler", "terminal"),
    ("interaction-handler", "interaction"),
];

/// 存量插件编号迁移（幂等，仅做文件操作，失败不阻塞装载）：
///
/// - 新编号已安装：把旧目录的 data/runtime/logs 并入新目录（不覆盖已有
///   文件，保留会话等用户数据），旧目录整体归档到 `.legacy-plugins/`；
/// - 新编号未安装：给旧目录打禁用标记，避免半失联的工具继续注册；
///   用户安装新编号插件后，下次启动自动完成数据并入。
pub fn migrate_legacy_plugin_ids(storage_root: &Path) {
    for (legacy_id, current_id) in LEGACY_PLUGIN_ID_MAP {
        let legacy_dir = plugin_directory(storage_root, legacy_id);
        if !legacy_dir.join(MANIFEST_FILE).is_file() {
            continue;
        }
        let current_dir = plugin_directory(storage_root, current_id);
        if current_dir.join(MANIFEST_FILE).is_file() {
            merge_preserved_entries(&legacy_dir, &current_dir);
            archive_legacy_directory(storage_root, legacy_id);
            tracing::info!(legacy_id, current_id, "旧编号插件数据已并入新编号并归档");
        } else if !legacy_dir.join(DISABLED_MARKER).is_file() {
            match std::fs::write(legacy_dir.join(DISABLED_MARKER), "旧编号插件待迁移\n") {
                Ok(()) => tracing::warn!(
                    legacy_id,
                    current_id,
                    "旧编号插件存在而新编号未安装，已禁用；安装新编号插件后将自动并入数据"
                ),
                Err(error) => {
                    tracing::warn!(%error, legacy_id, "标记旧编号插件禁用失败")
                }
            }
        }
    }
}

/// 把旧目录中安装过程需保留的三个子目录并入新目录，不覆盖已有文件。
fn merge_preserved_entries(legacy_dir: &Path, current_dir: &Path) {
    for entry in PRESERVED_ENTRIES {
        let source = legacy_dir.join(entry);
        if !source.is_dir() {
            continue;
        }
        if let Err(error) = merge_directory_contents(&source, &current_dir.join(entry)) {
            tracing::warn!(%error, entry, "并入旧编号插件数据失败");
        }
    }
}

fn merge_directory_contents(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)
        .with_context(|| format!("创建目录失败: {}", target.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("读取目录失败: {}", source.display()))?
    {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.path().is_dir() {
            merge_directory_contents(&entry.path(), &destination)?;
        } else if !destination.exists() {
            std::fs::copy(entry.path(), &destination)
                .with_context(|| format!("复制文件失败: {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// 把旧编号插件目录整体移入 `.legacy-plugins/` 归档（移出插件扫描范围）。
fn archive_legacy_directory(storage_root: &Path, legacy_id: &str) {
    let legacy_dir = plugin_directory(storage_root, legacy_id);
    let archive_root = storage_root.join(".legacy-plugins");
    let mut target = archive_root.join(legacy_id);
    if target.exists() {
        // 归档位已占用（如用户手动恢复过旧目录）：追加时间戳避免覆盖。
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        target = archive_root.join(format!("{legacy_id}-{stamp}"));
    }
    if let Err(error) =
        std::fs::create_dir_all(&archive_root).and_then(|()| std::fs::rename(&legacy_dir, &target))
    {
        tracing::warn!(%error, legacy_id, "归档旧编号插件目录失败");
    }
}

#[derive(Clone)]
struct InstalledPlugin {
    directory: PathBuf,
    manifest: PluginManifest,
    enabled: bool,
    signed_release: Option<SignedPluginRelease>,
}

struct LoadedPlugin {
    directory: PathBuf,
    manifest: PluginManifest,
    wasm_bytes: Option<Arc<Vec<u8>>>,
    component: Option<Arc<wasmtime::component::Component>>,
    ui_plugin: Option<Arc<Mutex<WasmPlugin>>>,
    descriptor: Option<Descriptor>,
    generation: u64,
    instances: Vec<Weak<WasmPluginAdapter>>,
    ts_instances: Vec<Weak<TsPluginAdapter>>,
    sidecar: Option<Arc<dyn SidecarConnection>>,
    last_error: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatus {
    pub id: String,
    pub name: String,
    pub manifest_version: String,
    pub loaded_version: Option<String>,
    pub state: String,
    pub generation: u64,
    pub enabled: bool,
    pub can_rollback: bool,
    pub has_sidecar: bool,
    pub sidecar_running: bool,
    pub last_error: Option<String>,
    /// 插件未注册为可调用工具的原因（如缺少模型能力、入口不匹配）。
    /// None 表示插件当前可调用；Some 表示不可调用及原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// 扫描时被忽略的无效插件记录。
///
/// `id` 为插件目录名（可能与清单 ID 不同，如部署残留的 `.terminal-staging-*`），
/// 是清理操作的唯一标识；清单可读时附带名称与版本，便于界面展示。
#[derive(Debug, Clone, Serialize)]
pub struct InvalidPluginEntry {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<String>,
    pub reason: String,
}

fn loaded_plugins() -> &'static Mutex<HashMap<String, LoadedPlugin>> {
    LOADED_PLUGINS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn invalid_plugins() -> &'static Mutex<Vec<InvalidPluginEntry>> {
    INVALID_PLUGINS.get_or_init(|| Mutex::new(Vec::new()))
}

fn sidecar_connections() -> &'static Mutex<HashMap<PathBuf, Arc<dyn SidecarConnection>>> {
    SIDECAR_CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 宿主退出时逐个停止所有已启动的 sidecar。
///
/// sidecar 经 `setsid()` 脱离进程组独立运行，不会随宿主自动退出。
/// 宿主必须在退出前调用本函数主动终止它们，否则会残留孤儿进程占用端口与资源。
pub fn shutdown_all_sidecars() {
    crate::ts_tools::cancel_all_calls();
    let connections = sidecar_connections()
        .lock()
        .map(|connections| connections.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let total = connections.len();
    let mut stopped = 0;
    for connection in connections {
        let plugin_id = connection.plugin_id().to_string();
        match connection.stop() {
            Ok(()) => {
                stopped += 1;
                tracing::info!(plugin_id = %plugin_id, "sidecar 已停止");
            }
            Err(error) => {
                // endpoint 文件不存在说明 sidecar 未启动或已退出，属正常情况。
                tracing::debug!(plugin_id = %plugin_id, %error, "停止 sidecar 时无需操作（可能未运行）");
            }
        }
    }
    tracing::info!(total, stopped, "sidecar 关闭完成");
}

/// 预加载设置页实例，供尚未创建 Core 时查询插件贡献。
pub fn preload_installed_plugins(storage_root: &Path) -> usize {
    migrate_legacy_plugin_ids(storage_root);

    let Ok(_operation) = LOAD_OPERATION.lock() else {
        tracing::warn!("插件加载操作锁已损坏");
        return 0;
    };

    let (installed_plugins, discovered_invalid) = discover_installed_plugins(storage_root);
    if let Ok(mut registered) = invalid_plugins().lock() {
        *registered = discovered_invalid;
    }
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

/// 若 `plugin_id` 是已登记的无效插件目录则删除它并返回 true。
///
/// 无效插件从未进入注册表，没有 sidecar 或 WASM 实例需要停止；但删除路径
/// 与正常卸载一致（事务目录暂存、可选保留 data）。登记表中的 id 均来自
/// 实际扫描到的目录名，天然不含路径分隔符，不会越出插件根目录。
fn remove_invalid_plugin_if_registered(
    storage_root: &Path,
    plugin_id: &str,
    keep_data: bool,
) -> Result<bool> {
    let registered = invalid_plugins()
        .lock()
        .map_err(|_| anyhow::anyhow!("无效插件登记表已损坏"))?
        .clone();
    if !registered.iter().any(|entry| entry.id == plugin_id) {
        return Ok(false);
    }
    let directory = plugins_directory(storage_root).join(plugin_id);
    let removed = transaction_directory(storage_root, "uninstall-invalid")?;
    let result = (|| -> Result<()> {
        if !directory.is_dir() {
            // 目录已不存在（如用户手动删除）：仅同步登记表。
            return Ok(());
        }
        ensure_directory(&directory)?;
        rename_with_retry(&directory, &removed)?;
        let uninstall_result = if keep_data {
            preserve_only_data(&removed, &directory)
        } else {
            remove_directory_if_exists(&removed)
        };
        if let Err(error) = uninstall_result {
            // 无效插件无运行时状态，恢复目录即可让用户重试。
            let _ = remove_directory_if_exists(&directory);
            let _ = rename_with_retry(&removed, &directory);
            return Err(error);
        }
        Ok(())
    })();
    invalid_plugins()
        .lock()
        .map_err(|_| anyhow::anyhow!("无效插件登记表已损坏"))?
        .retain(|entry| entry.id != plugin_id);
    result?;
    tracing::info!(plugin_id, keep_data, "已清理无效插件目录");
    Ok(true)
}

/// 为一个 Core 创建独立实例。实例使用注册表已接受的同一份 WASM 字节快照。
///
/// `runtime` 用于按入口过滤：插件声明了 `entrypoints` 但不含当前入口时不注册。
/// 同时按 `model_requirements` 过滤：必需模型能力未配置时不注册工具（插件保持已安装）。
pub fn load_installed_plugins(_storage_root: &Path, runtime: RuntimeKind) -> Vec<Arc<dyn Plugin>> {
    let Ok(_operation) = LOAD_OPERATION.lock() else {
        tracing::warn!("插件加载操作锁已损坏");
        return Vec::new();
    };

    let configured = configured_model_capabilities();
    let plugin_ids = {
        let Ok(plugins) = loaded_plugins().lock() else {
            return Vec::new();
        };
        plugins
            .values()
            .filter_map(|loaded| {
                if let Some(reason) =
                    check_plugin_availability(&loaded.manifest, runtime, &configured)
                {
                    tracing::info!(
                        plugin_id = %loaded.manifest.id,
                        reason,
                        "插件未注册工具（仍保持已安装）"
                    );
                    None
                } else {
                    Some(loaded.manifest.id.clone())
                }
            })
            .collect::<Vec<_>>()
    };

    plugin_ids
        .into_iter()
        .filter_map(|plugin_id| load_core_plugin(&plugin_id, runtime))
        .collect()
}

/// 返回已安装插件状态，并探测 sidecar 当前是否可用。
///
/// `runtime` 用于判断插件是否可在当前入口注册工具，填充 `unavailable_reason`。
pub fn list_plugins(_storage_root: &Path, runtime: RuntimeKind) -> Vec<PluginStatus> {
    let configured = configured_model_capabilities();
    let Ok(plugins) = loaded_plugins().lock() else {
        return Vec::new();
    };
    let mut statuses = plugins
        .values()
        .map(|loaded| {
            let manifest = &loaded.manifest;
            let sidecar_running = loaded
                .sidecar
                .as_ref()
                .is_some_and(|connection| connection.has_runtime_endpoint());
            let state = plugin_state(
                manifest,
                loaded.enabled,
                loaded.ui_plugin.is_some(),
                loaded.last_error.as_deref(),
            );
            PluginStatus {
                unavailable_reason: if loaded.enabled {
                    check_plugin_availability(manifest, runtime, &configured)
                } else {
                    None
                },
                id: manifest.id.clone(),
                name: loaded
                    .descriptor
                    .as_ref()
                    .map(|value| value.name.clone())
                    .unwrap_or_else(|| manifest.id.clone()),
                manifest_version: manifest.version.clone(),
                loaded_version: loaded
                    .descriptor
                    .as_ref()
                    .map(|value| value.version.clone()),
                state: state.to_string(),
                generation: loaded.generation,
                enabled: loaded.enabled,
                can_rollback: rollback_directory(&loaded.directory, &manifest.id).is_dir(),
                has_sidecar: manifest.sidecar.is_some(),
                sidecar_running,
                last_error: loaded.last_error.clone(),
            }
        })
        .collect::<Vec<_>>();
    // 无效插件（签名无效/沙箱越权/清单损坏）以 invalid 状态并列展示，供用户清理。
    if let Ok(invalid) = invalid_plugins().lock() {
        for entry in invalid.iter() {
            statuses.push(PluginStatus {
                id: entry.id.clone(),
                name: entry.name.clone(),
                manifest_version: entry.manifest_version.clone().unwrap_or_default(),
                loaded_version: None,
                state: "invalid".to_string(),
                generation: 0,
                enabled: false,
                can_rollback: false,
                has_sidecar: false,
                sidecar_running: false,
                last_error: Some(entry.reason.clone()),
                unavailable_reason: None,
            });
        }
    }
    statuses.sort_by(|left, right| left.id.cmp(&right.id));
    statuses
}

/// 从磁盘读取插件新版本。全部 UI/Core 实例成功创建后才切换。
pub fn reload_plugin(storage_root: &Path, plugin_id: &str) -> Result<PluginStatus> {
    let _operation = LOAD_OPERATION
        .lock()
        .map_err(|_| anyhow::anyhow!("插件加载操作锁已损坏"))?;
    let installed = find_installed_plugin(storage_root, plugin_id)?;
    if loaded_plugin_matches(&installed)? {
        return list_plugin_status_without_preload(&installed.manifest)
            .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 当前状态丢失"));
    }

    let result = reload_plugin_inner(storage_root, &installed);
    if let Err(error) = &result {
        set_last_error(plugin_id, error.to_string());
    }
    result?;

    list_plugin_status_without_preload(&installed.manifest)
        .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 热加载后状态丢失"))
}

fn reload_plugin_inner(storage_root: &Path, installed: &InstalledPlugin) -> Result<()> {
    // 无 WASM 插件可能仅提供 UI，也可能通过 Desktop TS 工具适配器接入 Core。
    // UI 记录直接替换；存活 Core 中的 TS 适配器原位更新，下一轮立即使用新清单。
    if installed.manifest.wasm_binary().is_none() {
        crate::ts_tools::cancel_plugin_calls(&installed.manifest.id);
        let ts_instances = loaded_plugins()
            .lock()
            .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?
            .get(&installed.manifest.id)
            .map(|loaded| {
                loaded
                    .ts_instances
                    .iter()
                    .filter_map(Weak::upgrade)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for adapter in &ts_instances {
            adapter.reconfigure(&installed.manifest, installed.enabled);
        }
        let mut loaded = load_plugin_record(storage_root, installed.clone());
        loaded.ts_instances = ts_instances.iter().map(Arc::downgrade).collect();
        loaded_plugins()
            .lock()
            .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?
            .insert(installed.manifest.id.clone(), loaded);
        tracing::info!(plugin_id = %installed.manifest.id, "无 WASM 插件已重新加载");
        return Ok(());
    }

    let wasm_bytes = Arc::new(read_wasm_bytes(installed)?);
    let sidecar = resolve_sidecar(storage_root, installed, true)?;
    // Command sidecar 等 Core 汇总 exec_env 后再首次启动；其他常驻 sidecar
    //（如 scheduler）继续在预加载/热加载阶段启动。
    if installed.enabled
        && installed.manifest.id != "command"
        && let Some(connection) = &sidecar
    {
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
    let (component, ui_plugin, descriptor) = compile_plugin(
        wasm_bytes.clone(),
        &installed.manifest,
        sidecar.clone(),
        runtime_config,
    )?;

    let mut replacements = Vec::with_capacity(instances.len());
    for adapter in &instances {
        let component = component.clone().expect("热加载替换仅适用于带逻辑层的插件");
        let plugin = instantiate_from_compiled(
            component,
            sidecar.clone(),
            adapter.runtime_config(),
            installed.manifest.id.clone(),
            installed.manifest.storage_access,
        )?;
        let adapter = adapter.clone();
        let activate = installed.enabled;
        let replacement = crate::execution::run_outside_tokio(move || {
            adapter.prepare_replacement(plugin, activate)
        })?;
        replacements.push(replacement);
    }

    for (adapter, replacement) in instances.iter().zip(replacements) {
        adapter.replace_inner(replacement);
        adapter.set_enabled(installed.enabled);
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
    loaded.component = component;
    loaded.ui_plugin = ui_plugin.map(|plugin| Arc::new(Mutex::new(plugin)));
    loaded.descriptor = descriptor;
    loaded.generation = next_generation;
    loaded.instances = instances.iter().map(Arc::downgrade).collect();
    loaded.sidecar = sidecar;
    loaded.last_error = None;
    loaded.enabled = installed.enabled;
    tracing::info!(
        plugin_id = %installed.manifest.id,
        generation = next_generation,
        instances = instances.len(),
        "WASM 插件热加载完成"
    );
    Ok(())
}

/// 通过插件 ID 调用其 sidecar，入口不需要了解制品位置或传输协议。
/// 校验本地信任（L3）登记目标：插件已安装、目录一致且未携带官方签名。
/// 供宿主信任登记命令在用户确认前调用（RFC 0017 D3）。
pub fn verify_local_trust_target(
    storage_root: &Path,
    plugin_id: &str,
    directory: &Path,
) -> Result<()> {
    let installed = find_installed_plugin(storage_root, plugin_id)?;
    if installed.directory != directory {
        bail!("目录与插件 {plugin_id} 的安装目录不一致，拒绝登记本地信任");
    }
    if installed.signed_release.is_some() {
        bail!("插件 {plugin_id} 已携带官方签名，无需本地信任");
    }
    Ok(())
}

/// 一次性 command sidecar 路由（透明执行封套第一阶段）：
/// 预分类 → 票据核验 → 按会话工作区构造策略 → 沙箱/全权实例执行 →
/// 违规提示附加。插件侧无任何沙箱逻辑，协议原样透传。
fn invoke_command_ephemeral(
    storage_root: &Path,
    installed: &InstalledPlugin,
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    use tiangong_sandbox::escalation::verify_and_strip_escalation;

    let (payload, escalation_granted) = verify_and_strip_escalation(operation, payload);
    let full_trust = payload
        .get("access")
        .and_then(|access| access.get("full_trust"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let full_access = escalation_granted || full_trust;

    // 预分类：非全权时拒绝已知高危命令（引导走审批通道）。
    if !full_access {
        let dangerous = match operation {
            "command.run_shell" => payload
                .get("script")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|script| {
                    tiangong_sandbox::assess_script(script)
                        == tiangong_sandbox::CommandRisk::KnownDangerous
                }),
            _ => {
                let cmd = payload
                    .get("cmd")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let args = payload
                    .get("args")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(String::from)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                tiangong_sandbox::assess_program(cmd, &args)
                    == tiangong_sandbox::CommandRisk::KnownDangerous
            }
        };
        if dangerous {
            bail!(
                "命令被宿主预分类器判定为高危，未执行。如确需执行，请先调用                  request_user（kind: approval）获得用户批准，再经界面批准的升级                  票据以全权方式执行。"
            );
        }
    }

    // 会话工作区（策略可写根）：请求自带，插件无需感知。
    let workspace = payload
        .get("access")
        .and_then(|access| access.get("workspace"))
        .or_else(|| payload.get("cwd"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && Path::new(value).is_dir())
        .map(PathBuf::from)
        .unwrap_or_else(|| installed.directory.join("data"));

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
    let base = installed
        .directory
        .join(format!("ephemeral-{}", scru128::new()));
    let config = SidecarConfig::new(
        &installed.manifest.id,
        &installed.manifest.version,
        binary,
        base.join("endpoint.json"),
        base.join("sidecar.log"),
        base.join("data"),
        storage_root.to_path_buf(),
    )
    .with_timeouts(
        Duration::from_millis(sidecar.startup_timeout_ms),
        Duration::from_millis(sidecar.request_timeout_ms),
    )
    // 全权实例不套沙箱（用户已批准的通道）；其余按会话工作区沙箱执行，
    // 平台沙箱不可用时 spawn 显式失败（不静默降级裸奔）。
    .with_sandbox(!full_access)
    .with_sandbox_workspace(if full_access { None } else { Some(workspace) });

    let connection = StdioSidecarConnection::new(config);
    let payload_text = serde_json::to_string(&payload).with_context(|| "序列化插件请求失败")?;
    let invoke_result = connection.invoke(operation, &payload_text);
    let response_text = match invoke_result {
        Ok(text) => text,
        Err(error) => {
            let _ = connection.stop();
            let _ = std::fs::remove_dir_all(&base);
            return Err(error);
        }
    };
    let _ = connection.stop();
    let _ = std::fs::remove_dir_all(&base);

    let mut response: serde_json::Value =
        serde_json::from_str(&response_text).with_context(|| "解析插件响应失败")?;
    // 沙箱违规归因提示（对模型透明，Agent 可自主改写法或申请升级）。
    if !full_access
        && response.get("ok").and_then(serde_json::Value::as_bool) == Some(false)
        && let Some(hint) = response
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .and_then(tiangong_sandbox::explain_violation)
    {
        let annotated = response
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(map) = response.as_object_mut() {
            map.insert(
                "stderr".to_string(),
                serde_json::Value::String(format!("{annotated}\n[沙箱提示] {hint}")),
            );
        }
    }
    Ok(response)
}

pub fn invoke_sidecar(
    storage_root: &Path,
    plugin_id: &str,
    operation: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let installed = find_installed_plugin(storage_root, plugin_id)?;
    if !installed.enabled {
        bail!("插件 {plugin_id} 已停用");
    }
    // 命令执行操作走一次性沙箱实例（透明执行封套）：宿主按请求自带的
    // 会话工作区构造策略，每次调用启动独立 command sidecar，插件零感知。
    if plugin_id == "command" && matches!(operation, "command.run_command" | "command.run_shell") {
        return invoke_command_ephemeral(storage_root, &installed, operation, payload);
    }
    let connection = sidecar_connection(storage_root, &installed, false)?;
    let payload = serde_json::to_string(&payload).with_context(|| "序列化插件请求失败")?;
    let response = connection.invoke(operation, &payload)?;
    serde_json::from_str(&response).with_context(|| "解析插件响应失败")
}

/// 取已启用插件的安装目录（供桥接层访问插件私有数据）。
pub fn plugin_install_directory(plugin_id: &str) -> Option<PathBuf> {
    let plugins = loaded_plugins().lock().ok()?;
    let loaded = plugins.get(plugin_id)?;
    loaded.enabled.then(|| loaded.directory.clone())
}

/// 取已启用插件的 manifest 快照（供桥接层做权限校验）。
pub fn plugin_manifest(plugin_id: &str) -> Option<PluginManifest> {
    let plugins = loaded_plugins().lock().ok()?;
    let loaded = plugins.get(plugin_id)?;
    loaded.enabled.then(|| loaded.manifest.clone())
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
                if !loaded.enabled {
                    return None;
                }
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

/// 按 Slot 列出 UI 贡献（宿主 UI 接缝的统一查询入口）。
///
/// - v1 插件：WASM 运行时声明的设置页贡献整体映射到 `settings.plugin-page`，
///   零改动兼容（设计文档 11）。
/// - v2 插件：manifest `ui.contributions` 中 slot 匹配的项。
pub fn list_slot_contributions(slot: &str) -> Vec<SlotContribution> {
    let mut result = Vec::new();
    let Ok(plugins) = loaded_plugins().lock() else {
        return result;
    };
    for (plugin_id, loaded) in plugins.iter() {
        if !loaded.enabled {
            continue;
        }
        // v2：manifest 声明的贡献
        for contribution in loaded.manifest.ui_contributions() {
            if contribution.slot == slot {
                result.push(SlotContribution {
                    plugin_id: plugin_id.clone(),
                    contribution_id: contribution.id.clone(),
                    slot: contribution.slot.clone(),
                    title: contribution.title.clone(),
                    description: contribution.description.clone(),
                    icon: contribution.icon.clone(),
                    group: String::new(),
                    has_view: true,
                    open_mode: contribution.open_mode,
                    sandbox: contribution.sandbox,
                    source: ContributionSource::Manifest,
                });
            }
        }
        // v1：WASM 设置页贡献映射到 settings.plugin-page
        if loaded.manifest.schema_version == 1
            && slot == "settings.plugin-page"
            && let Some(ui_plugin) = &loaded.ui_plugin
            && let Ok(contributions) =
                call_wasm_off_runtime(ui_plugin.clone(), WasmPlugin::contributions)
        {
            for contribution in contributions {
                result.push(SlotContribution {
                    plugin_id: plugin_id.clone(),
                    contribution_id: contribution.id.clone(),
                    slot: slot.to_string(),
                    title: contribution.title.clone(),
                    description: contribution.description.clone(),
                    icon: contribution.icon.clone(),
                    group: contribution.group.clone(),
                    has_view: contribution.has_view,
                    open_mode: crate::slots::OpenMode::Singleton,
                    sandbox: crate::slots::SandboxKind::Iframe,
                    source: ContributionSource::Wasm,
                });
            }
        }
    }
    result.sort_by(|left, right| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then(left.contribution_id.cmp(&right.contribution_id))
    });
    result
}

/// 读取 v2 manifest UI 贡献声明的入口 HTML 文件。
///
/// v1 贡献的页面由 WASM `open-view` 提供（见 [`open_view`]），本函数只服务
/// manifest 声明的 `entry`。
pub fn open_manifest_view(plugin_id: &str, contribution_id: &str) -> Result<String> {
    let (directory, entry) = {
        let plugins = loaded_plugins()
            .lock()
            .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?;
        let loaded = plugins
            .get(plugin_id)
            .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 未加载"))?;
        let contribution = loaded
            .manifest
            .ui_contributions()
            .into_iter()
            .find(|item| item.id == contribution_id)
            .ok_or_else(|| {
                anyhow::anyhow!("插件 {plugin_id} 无 manifest 贡献 {contribution_id}")
            })?;
        (loaded.directory.clone(), contribution.entry)
    };
    let path = directory.join(&entry);
    std::fs::read_to_string(&path).with_context(|| format!("读取插件页面失败: {}", path.display()))
}

/// 读取 v2 manifest UI 贡献的相对资源文件（以 entry 所在目录为根）。
///
/// 供 Shadow/iframe 容器加载入口 HTML 引用的脚本与样式：路径按 Web 相对语义
/// 解析（`./`、子目录），规范化后不得逃出插件安装目录。MIME 按扩展名推断。
pub fn read_manifest_resource(
    plugin_id: &str,
    contribution_id: &str,
    path: &str,
) -> Result<(Vec<u8>, String)> {
    let (directory, entry) = {
        let plugins = loaded_plugins()
            .lock()
            .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?;
        let loaded = plugins
            .get(plugin_id)
            .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 未加载"))?;
        let contribution = loaded
            .manifest
            .ui_contributions()
            .into_iter()
            .find(|item| item.id == contribution_id)
            .ok_or_else(|| {
                anyhow::anyhow!("插件 {plugin_id} 无 manifest 贡献 {contribution_id}")
            })?;
        (loaded.directory.clone(), contribution.entry)
    };

    let base_dir = directory
        .join(&entry)
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| directory.clone());
    let resource = base_dir.join(path);
    // 规范化后必须仍在插件目录内，拒绝 `../` 逃逸。
    let resolved = resource
        .canonicalize()
        .with_context(|| format!("资源路径无效: {path}"))?;
    let plugin_root = directory
        .canonicalize()
        .with_context(|| format!("插件目录无效: {}", directory.display()))?;
    if !resolved.starts_with(&plugin_root) {
        bail!("插件 {plugin_id} 资源路径 {path} 逃出插件目录，已拒绝");
    }
    let bytes = std::fs::read(&resolved)
        .with_context(|| format!("读取插件资源失败: {}", resolved.display()))?;
    Ok((bytes, mime_of(&resolved)))
}

/// 按扩展名推断资源 MIME（容器加载脚本/样式用，未知类型按二进制流返回）。
fn mime_of(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// 按 Slot 查询得到的统一 UI 贡献项。
#[derive(Debug, Clone, Serialize)]
pub struct SlotContribution {
    pub plugin_id: String,
    pub contribution_id: String,
    pub slot: String,
    pub title: String,
    pub description: String,
    pub icon: String,
    pub group: String,
    /// 是否有可渲染页面（v1 由 WASM 声明；v2 manifest 贡献恒有）。
    pub has_view: bool,
    pub open_mode: crate::slots::OpenMode,
    pub sandbox: crate::slots::SandboxKind,
    /// 贡献来源：WASM 运行时声明（v1）或 manifest 声明（v2）。
    pub source: ContributionSource,
}

/// UI 贡献的声明来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionSource {
    /// v1：WASM `contributions()` 运行时声明。
    Wasm,
    /// v2：manifest `ui.contributions` 声明。
    Manifest,
}

/// 拓展区 App 元数据：声明 `extension.tab` 贡献的插件即可作为 App 打开
/// （设计文档 6.6）。目录完全由已安装插件的贡献驱动，不在代码里写死；
/// 官方内置能力（浏览器/终端/Agent Team）后续以插件形态注册，装上即出现。
#[derive(Debug, Clone, Serialize)]
pub struct ExtensionApp {
    pub plugin_id: String,
    pub contribution_id: String,
    /// 插件 descriptor 名称（矩阵主标题）。
    pub name: String,
    /// 贡献标题（缺省回落 plugin_id）。
    pub title: String,
    pub description: String,
    pub icon: String,
    /// singleton：全局至多一个 tab，重复打开聚焦；multi：每次打开新建。
    pub open_mode: crate::slots::OpenMode,
    pub sandbox: crate::slots::SandboxKind,
}

/// 列出全部可打开的拓展区 App：聚合已启用插件 manifest 中 slot 为
/// `extension.tab` 的贡献与插件 descriptor 名称。v1 插件无 manifest UI
/// 贡献，不进入 App 列表。
pub fn list_extension_apps() -> Vec<ExtensionApp> {
    let mut apps = Vec::new();
    let Ok(plugins) = loaded_plugins().lock() else {
        return apps;
    };
    for (plugin_id, loaded) in plugins.iter() {
        if !loaded.enabled {
            continue;
        }
        let plugin_name = loaded
            .descriptor
            .as_ref()
            .map(|descriptor| descriptor.name.clone())
            .unwrap_or_else(|| plugin_id.clone());
        for contribution in loaded.manifest.ui_contributions() {
            if contribution.slot != "extension.tab" {
                continue;
            }
            apps.push(ExtensionApp {
                plugin_id: plugin_id.clone(),
                contribution_id: contribution.id.clone(),
                name: plugin_name.clone(),
                title: contribution.title.clone(),
                description: contribution.description.clone(),
                icon: contribution.icon.clone(),
                open_mode: contribution.open_mode,
                sandbox: contribution.sandbox,
            });
        }
    }
    apps.sort_by(|left, right| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then(left.contribution_id.cmp(&right.contribution_id))
    });
    apps
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
    handle_view_message_result(plugin_id, method, payload).ok()
}

/// 处理插件页面消息并保留 WASM/sidecar 返回的具体错误。
pub fn handle_view_message_result(plugin_id: &str, method: &str, payload: &str) -> Result<String> {
    let plugin = ui_plugin(plugin_id)
        .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 未加载、未启用或没有逻辑层"))?;
    let method = method.to_string();
    let payload = payload.to_string();
    call_wasm_off_runtime(plugin, move |plugin| {
        plugin.handle_view_message(method, payload)
    })
}

fn ui_plugin(plugin_id: &str) -> Option<Arc<Mutex<WasmPlugin>>> {
    let plugins = loaded_plugins().lock().ok()?;
    let loaded = plugins.get(plugin_id)?;
    loaded.enabled.then(|| loaded.ui_plugin.clone()).flatten()
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
        loader.load_bytes_for_plugin(&bytes, &config, &plugin_id_for_load, false)
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
    if installed.enabled
        && installed.manifest.id != "command"
        && let Some(connection) = &sidecar
        && let Err(error) = connection.ensure_running()
    {
        tracing::warn!(plugin_id = %installed.manifest.id, %error, "插件 sidecar 暂不可用");
        last_error = Some(error.to_string());
    }
    // 纯 UI 插件（wasm 省略）：无逻辑层，直接构造已加载记录
    let load_result = if installed.manifest.wasm_binary().is_some() {
        read_wasm_bytes(&installed)
            .map(|bytes| {
                let bytes = Arc::new(bytes);
                compile_plugin(
                    bytes.clone(),
                    &installed.manifest,
                    sidecar.clone(),
                    PluginRuntimeConfig::default(),
                )
                .map(|(component, plugin, descriptor)| (Some(bytes), component, plugin, descriptor))
            })
            .and_then(|result| result)
    } else {
        Ok((None, None, None, None))
    };

    match load_result {
        Ok((bytes, component, plugin, descriptor)) => {
            tracing::info!(plugin_id = %installed.manifest.id, "WASM 插件已预加载");
            LoadedPlugin {
                directory: installed.directory,
                manifest: installed.manifest,
                wasm_bytes: bytes,
                component,
                ui_plugin: plugin.map(|plugin| Arc::new(Mutex::new(plugin))),
                descriptor,
                generation: 1,
                instances: Vec::new(),
                ts_instances: Vec::new(),
                sidecar,
                last_error,
                enabled: installed.enabled,
            }
        }
        Err(error) => {
            tracing::warn!(plugin_id = %installed.manifest.id, %error, "加载 WASM 插件失败");
            LoadedPlugin {
                directory: installed.directory,
                manifest: installed.manifest,
                wasm_bytes: None,
                component: None,
                ui_plugin: None,
                descriptor: None,
                generation: 0,
                instances: Vec::new(),
                ts_instances: Vec::new(),
                sidecar,
                last_error: Some(error.to_string()),
                enabled: installed.enabled,
            }
        }
    }
}

fn load_core_plugin(plugin_id: &str, runtime: RuntimeKind) -> Option<Arc<dyn Plugin>> {
    let (manifest, component, descriptor_id, sidecar, enabled, storage_access) = {
        let plugins = loaded_plugins().lock().ok()?;
        let loaded = plugins.get(plugin_id)?;
        (
            loaded.manifest.clone(),
            loaded.component.clone(),
            loaded
                .descriptor
                .as_ref()
                .map(|descriptor| descriptor.id.clone())
                .unwrap_or_else(|| plugin_id.to_string()),
            loaded.sidecar.clone(),
            loaded.enabled,
            loaded.manifest.storage_access,
        )
    };

    let has_ts_contributions = manifest
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
        || manifest
            .prompt
            .as_ref()
            .is_some_and(|prompts| !prompts.is_empty());
    if runtime == RuntimeKind::Desktop && has_ts_contributions {
        let adapter = Arc::new(TsPluginAdapter::from_manifest(&manifest, enabled));
        if let Ok(mut plugins) = loaded_plugins().lock()
            && let Some(loaded) = plugins.get_mut(plugin_id)
        {
            loaded
                .ts_instances
                .retain(|instance| instance.strong_count() > 0);
            loaded.ts_instances.push(Arc::downgrade(&adapter));
        }
        return Some(adapter);
    }

    let component = component?;
    let plugin = match instantiate_from_compiled(
        component,
        sidecar.clone(),
        PluginRuntimeConfig::default(),
        plugin_id.to_string(),
        storage_access,
    ) {
        Ok(plugin) => plugin,
        Err(error) => {
            set_last_error(plugin_id, error.to_string());
            tracing::warn!(plugin_id, %error, "创建 Core WASM 插件实例失败");
            return None;
        }
    };
    let adapter = Arc::new(WasmPluginAdapter::new_with_id(
        plugin,
        PluginRuntimeConfig::default(),
        enabled,
        descriptor_id,
        sidecar.map(|s| s as Arc<dyn SidecarConnection>),
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

/// 编译产物：纯 UI 插件（无 wasm）三项均为 None。
type CompiledPlugin = (
    Option<Arc<wasmtime::component::Component>>,
    Option<WasmPlugin>,
    Option<Descriptor>,
);

fn compile_plugin(
    bytes: Arc<Vec<u8>>,
    manifest: &PluginManifest,
    sidecar: Option<Arc<dyn SidecarConnection>>,
    config: PluginRuntimeConfig,
) -> Result<CompiledPlugin> {
    // 纯 UI 插件（wasm 省略）：无逻辑层，返回空三元组
    if manifest.wasm_binary().is_none() {
        return Ok((None, None, None));
    }
    let plugin_id = manifest.id.clone();
    let expected_version = manifest.version.clone();
    crate::execution::run_outside_tokio(move || {
        let component = Arc::new(compile_component(&bytes)?);
        let mut plugin = instantiate_component(
            &component,
            &config,
            sidecar,
            &plugin_id,
            manifest.storage_access,
        )?;
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
        Ok((Some(component), Some(plugin), Some(descriptor)))
    })
}

fn instantiate_from_compiled(
    component: Arc<wasmtime::component::Component>,
    sidecar: Option<Arc<dyn SidecarConnection>>,
    config: PluginRuntimeConfig,
    plugin_id: String,
    storage_access: bool,
) -> Result<WasmPlugin> {
    crate::execution::run_outside_tokio(move || {
        instantiate_component(&component, &config, sidecar, &plugin_id, storage_access)
    })
}

fn read_wasm_bytes(installed: &InstalledPlugin) -> Result<Vec<u8>> {
    let wasm_binary = installed.manifest.wasm_binary().ok_or_else(|| {
        anyhow::anyhow!(
            "插件 {} 是纯 UI 插件，没有 WASM 制品",
            installed.manifest.id
        )
    })?;
    let path = installed.directory.join(wasm_binary);
    std::fs::read(&path).with_context(|| format!("读取插件 WASM 制品失败: {}", path.display()))
}

fn set_last_error(plugin_id: &str, error: String) {
    if let Ok(mut plugins) = loaded_plugins().lock()
        && let Some(plugin) = plugins.get_mut(plugin_id)
    {
        plugin.last_error = Some(error);
    }
}

fn plugin_state(
    manifest: &PluginManifest,
    enabled: bool,
    has_wasm_ui: bool,
    last_error: Option<&str>,
) -> &'static str {
    // 未运行的 sidecar 可能仍在等待首次调用，只有已记录的错误才影响插件状态。
    if !enabled {
        "disabled"
    } else if !has_wasm_ui && manifest.wasm_binary().is_some() {
        "error"
    } else if last_error.is_some() {
        "degraded"
    } else {
        "loaded"
    }
}

fn list_plugin_status_without_preload(manifest: &PluginManifest) -> Option<PluginStatus> {
    let (descriptor, generation, sidecar, last_error, has_ui, enabled, directory) = {
        let plugins = loaded_plugins().lock().ok()?;
        let loaded = plugins.get(&manifest.id)?;
        (
            loaded.descriptor.clone(),
            loaded.generation,
            loaded.sidecar.clone(),
            loaded.last_error.clone(),
            loaded.ui_plugin.is_some(),
            loaded.enabled,
            loaded.directory.clone(),
        )
    };
    let sidecar_running = sidecar
        .as_ref()
        .is_some_and(|connection| connection.has_runtime_endpoint());
    let state = plugin_state(manifest, enabled, has_ui, last_error.as_deref());
    let configured = configured_model_capabilities();
    Some(PluginStatus {
        unavailable_reason: if enabled {
            check_plugin_availability(manifest, RuntimeKind::Desktop, &configured)
        } else {
            None
        },
        id: manifest.id.clone(),
        name: descriptor
            .as_ref()
            .map(|value| value.name.clone())
            .unwrap_or_else(|| manifest.id.clone()),
        manifest_version: manifest.version.clone(),
        loaded_version: descriptor.map(|value| value.version),
        state: state.to_string(),
        generation,
        enabled,
        can_rollback: rollback_directory(&directory, &manifest.id).is_dir(),
        has_sidecar: manifest.sidecar.is_some(),
        sidecar_running,
        last_error,
    })
}

/// 将已下载并校验的临时目录安装为新插件，或升级现有插件。
pub fn install_staged_plugin(storage_root: &Path, staged_path: &Path) -> Result<PluginStatus> {
    install_staged_plugin_inner(storage_root, staged_path, false)
}

/// 导入用户选择的本地插件；允许同版本重新导入，但不允许降级。
pub fn import_staged_plugin(storage_root: &Path, staged_path: &Path) -> Result<PluginStatus> {
    install_staged_plugin_inner(storage_root, staged_path, true)
}

fn install_staged_plugin_inner(
    storage_root: &Path,
    staged_path: &Path,
    allow_same_version: bool,
) -> Result<PluginStatus> {
    let total_started = Instant::now();
    let lock_started = Instant::now();
    let _operation = LOAD_OPERATION
        .lock()
        .map_err(|_| anyhow::anyhow!("插件加载操作锁已损坏"))?;
    let lock_wait_ms = lock_started.elapsed().as_millis() as u64;

    let validation_started = Instant::now();
    let staged_result = validate_staged_plugin(storage_root, staged_path);
    let staged_validation_ms = validation_started.elapsed().as_millis() as u64;
    let staged = match staged_result {
        Ok(staged) => staged,
        Err(error) => {
            tracing::warn!(
                lock_wait_ms,
                staged_validation_ms,
                total_ms = total_started.elapsed().as_millis() as u64,
                %error,
                "插件安装目标校验失败"
            );
            return Err(error);
        }
    };
    let plugin_id = staged.manifest.id.clone();
    let destination = plugin_directory(storage_root, &staged.manifest.id);
    let lookup_started = Instant::now();
    let current = if destination.exists() {
        match find_installed_plugin(storage_root, &plugin_id) {
            Ok(installed) => Some(installed),
            Err(error) => {
                // 保留旧行为：无效残留目录交给 install_new_plugin 的恢复路径处理。
                tracing::warn!(
                    plugin_id,
                    %error,
                    "目标插件现有目录无效，按残留目录恢复路径处理"
                );
                None
            }
        }
    } else {
        None
    };
    let target_lookup_ms = lookup_started.elapsed().as_millis() as u64;

    let switch_started = Instant::now();
    let status = (|| {
        if let Some(current) = current {
            if current.directory != destination {
                bail!(
                    "插件 {} 安装目录与 ID 不一致: {}",
                    staged.manifest.id,
                    current.directory.display()
                );
            }
            ensure_installable_version(&current.manifest, &staged.manifest, allow_same_version)?;
            replace_installed_plugin(storage_root, staged_path, &current, staged.manifest.clone())
        } else {
            install_new_plugin(storage_root, staged_path, staged.manifest.clone())
        }
    })();
    let switch_ms = switch_started.elapsed().as_millis() as u64;
    // sidecar 二进制是新落盘文件：macOS 首次执行有一次性的安全评估
    // （实测约 1.6s）。导入完成后后台预热，避免这笔开销落到首次
    // 业务调用（打开终端 / 首次工具执行）上。
    if status.is_ok() {
        prewarm_plugin_sidecar(storage_root, &staged.manifest.id);
    }
    tracing::info!(
        plugin_id,
        allow_same_version,
        lock_wait_ms,
        staged_validation_ms,
        target_lookup_ms,
        switch_ms,
        total_ms = total_started.elapsed().as_millis() as u64,
        success = status.is_ok(),
        "插件安装运行时阶段完成"
    );
    status
}

/// 后台预热插件 sidecar：拉起进程并完成握手（幂等，已运行则即时返回）。
/// 失败不影响使用——首个业务调用会按原路径重试启动。
pub fn prewarm_plugin_sidecar(storage_root: &Path, plugin_id: &str) {
    let storage_root = storage_root.to_path_buf();
    let plugin_id = plugin_id.to_string();
    let spawned = std::thread::Builder::new()
        .name(format!("prewarm-sidecar-{plugin_id}"))
        .spawn(move || {
            let Ok(installed) = find_installed_plugin(&storage_root, &plugin_id) else {
                return;
            };
            if !installed.enabled || installed.manifest.sidecar.is_none() {
                return;
            }
            match sidecar_connection(&storage_root, &installed, false)
                .and_then(|connection| connection.ensure_running())
            {
                Ok(()) => tracing::info!(plugin_id, "插件 sidecar 预热完成"),
                Err(error) => {
                    tracing::debug!(plugin_id, %error, "插件 sidecar 预热失败（使用时重试）")
                }
            }
        });
    if let Err(error) = spawned {
        tracing::debug!(%error, "创建 sidecar 预热线程失败");
    }
}

/// 启用或停用插件，并立即同步所有存活 Core 实例。
pub fn set_plugin_enabled(
    storage_root: &Path,
    plugin_id: &str,
    enabled: bool,
) -> Result<PluginStatus> {
    let _operation = LOAD_OPERATION
        .lock()
        .map_err(|_| anyhow::anyhow!("插件加载操作锁已损坏"))?;
    let installed = find_installed_plugin(storage_root, plugin_id)?;
    if installed.enabled == enabled {
        return list_plugin_status_without_preload(&installed.manifest)
            .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 状态丢失"));
    }

    let marker = installed.directory.join(DISABLED_MARKER);

    // 无 WASM/sidecar 插件的启停只需更新标记、注册表与存活 TS 适配器。
    if installed.manifest.wasm_binary().is_none() && installed.manifest.sidecar.is_none() {
        if enabled {
            remove_file_if_exists(&marker)?;
        } else {
            create_disabled_marker(&marker)?;
            crate::ts_tools::cancel_plugin_calls(plugin_id);
            crate::bridge::clear_plugin_subscriptions(plugin_id);
        }
        let mut plugins = loaded_plugins()
            .lock()
            .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?;
        let loaded = plugins
            .get_mut(plugin_id)
            .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 尚未加载"))?;
        let ts_instances = loaded
            .ts_instances
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        loaded.enabled = enabled;
        loaded.last_error = None;
        drop(plugins);
        for adapter in ts_instances {
            adapter.set_enabled(enabled);
        }
        return list_plugin_status_without_preload(&installed.manifest)
            .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 状态丢失"));
    }

    if enabled {
        remove_file_if_exists(&marker)?;
        let mut enabled_plugin = installed.clone();
        enabled_plugin.enabled = true;
        if let Err(error) = reload_plugin_inner(storage_root, &enabled_plugin) {
            create_disabled_marker(&marker)?;
            set_last_error(plugin_id, error.to_string());
            return Err(error).with_context(|| format!("启用插件 {plugin_id} 失败"));
        }
    } else {
        create_disabled_marker(&marker)?;
        let (instances, ts_instances, sidecar) = {
            let mut plugins = loaded_plugins()
                .lock()
                .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?;
            let loaded = plugins
                .get_mut(plugin_id)
                .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 尚未加载"))?;
            loaded.enabled = false;
            loaded.last_error = None;
            let instances = loaded
                .instances
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            let ts_instances = loaded
                .ts_instances
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            (instances, ts_instances, loaded.sidecar.clone())
        };
        for adapter in &instances {
            adapter.set_enabled(false);
        }
        for adapter in &ts_instances {
            adapter.set_enabled(false);
        }
        crate::ts_tools::cancel_plugin_calls(plugin_id);
        crate::bridge::clear_plugin_subscriptions(plugin_id);
        if let Some(connection) = sidecar
            && let Err(error) = connection.stop()
        {
            for adapter in &instances {
                adapter.set_enabled(true);
            }
            for adapter in &ts_instances {
                adapter.set_enabled(true);
            }
            if let Ok(mut plugins) = loaded_plugins().lock()
                && let Some(loaded) = plugins.get_mut(plugin_id)
            {
                loaded.enabled = true;
                loaded.last_error = Some(error.to_string());
            }
            remove_file_if_exists(&marker)?;
            return Err(error).with_context(|| format!("停用插件 {plugin_id} 失败"));
        }
    }

    let mut refreshed = installed;
    refreshed.enabled = enabled;
    list_plugin_status_without_preload(&refreshed.manifest)
        .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 状态丢失"))
}

/// 将插件切换到本地保留的上一个版本，失败时恢复当前版本。
pub fn rollback_plugin(storage_root: &Path, plugin_id: &str) -> Result<PluginStatus> {
    let _operation = LOAD_OPERATION
        .lock()
        .map_err(|_| anyhow::anyhow!("插件加载操作锁已损坏"))?;
    let current = find_installed_plugin(storage_root, plugin_id)?;
    let rollback = rollback_directory(&current.directory, plugin_id);
    if !rollback.is_dir() {
        bail!("插件 {plugin_id} 没有可回滚版本");
    }

    stop_loaded_sidecar(plugin_id)?;
    crate::ts_tools::cancel_plugin_calls(plugin_id);
    let transaction = transaction_directory(storage_root, "rollback")?;
    swap_with_rollback(&current.directory, &rollback, &transaction, current.enabled)?;

    let rolled_back = find_installed_plugin(storage_root, plugin_id)?;
    if let Err(error) = reload_plugin_inner(storage_root, &rolled_back) {
        let _ = stop_connection_for_directory(&current.directory);
        let restore_transaction = transaction_directory(storage_root, "rollback-restore")?;
        let restore_result = swap_with_rollback(
            &current.directory,
            &rollback,
            &restore_transaction,
            current.enabled,
        )
        .and_then(|()| reload_plugin_inner(storage_root, &current));
        if let Err(restore_error) = restore_result {
            bail!("回滚插件 {plugin_id} 失败: {error}; 恢复当前版本失败: {restore_error}");
        }
        set_last_error(plugin_id, error.to_string());
        return Err(error).with_context(|| format!("回滚插件 {plugin_id} 失败"));
    }

    list_plugin_status_without_preload(&rolled_back.manifest)
        .ok_or_else(|| anyhow::anyhow!("插件 {plugin_id} 回滚后状态丢失"))
}

/// 卸载插件。保留数据时，安装目录中只留下 data 目录。
///
/// 无效插件目录（签名无效/沙箱越权/清单损坏）无法通过 `find_installed_plugin`
/// 校验，先查无效插件登记表，命中则直接走同一删除路径后返回。
pub fn uninstall_plugin(storage_root: &Path, plugin_id: &str, keep_data: bool) -> Result<()> {
    let _operation = LOAD_OPERATION
        .lock()
        .map_err(|_| anyhow::anyhow!("插件加载操作锁已损坏"))?;
    if remove_invalid_plugin_if_registered(storage_root, plugin_id, keep_data)? {
        return Ok(());
    }
    let installed = find_installed_plugin(storage_root, plugin_id)?;
    let expected = plugin_directory(storage_root, plugin_id);
    if installed.directory != expected {
        bail!(
            "插件 {plugin_id} 安装目录与 ID 不一致: {}",
            installed.directory.display()
        );
    }
    stop_loaded_sidecar(plugin_id)?;
    crate::ts_tools::cancel_plugin_calls(plugin_id);
    crate::bridge::clear_plugin_subscriptions(plugin_id);
    // 补齐连接表兜底：插件不在 loaded_plugins 时，仍可能保留在 sidecar 连接表中，
    // 不一并停止会导致 Windows 上二进制文件被占用、卸载删除失败。
    stop_connection_for_directory(&installed.directory)?;
    kill_sidecar_orphans(&installed);
    unload_plugin_wasm(plugin_id);

    let removed = transaction_directory(storage_root, "uninstall")?;
    rename_with_retry(&installed.directory, &removed)?;
    let uninstall_result = if keep_data {
        preserve_only_data(&removed, &installed.directory)
    } else {
        remove_directory_if_exists(&removed)
    };
    if let Err(error) = uninstall_result {
        tracing::error!(plugin_id, %error, "卸载插件删除目录失败，尝试恢复插件");
        let restore_result = (|| {
            remove_directory_if_exists(&installed.directory)?;
            rename_with_retry(&removed, &installed.directory)?;
            reload_plugin_inner(storage_root, &installed)
        })();
        if let Err(restore_error) = restore_result {
            bail!("卸载插件 {plugin_id} 失败: {error}; 恢复插件失败: {restore_error}");
        }
        return Err(error).with_context(|| format!("卸载插件 {plugin_id} 失败"));
    }

    let rollback = rollback_directory(&installed.directory, plugin_id);
    if let Err(error) = remove_directory_if_exists(&rollback) {
        tracing::warn!(path = %rollback.display(), %error, "插件已卸载，但清理回滚目录失败");
    }
    remove_sidecar_connection(&installed.directory);
    if let Ok(mut plugins) = loaded_plugins().lock()
        && let Some(loaded) = plugins.remove(plugin_id)
    {
        for adapter in loaded
            .instances
            .into_iter()
            .filter_map(|item| item.upgrade())
        {
            adapter.set_enabled(false);
        }
        for adapter in loaded
            .ts_instances
            .into_iter()
            .filter_map(|item| item.upgrade())
        {
            adapter.set_enabled(false);
        }
    }
    Ok(())
}

fn validate_staged_plugin(storage_root: &Path, staged_path: &Path) -> Result<InstalledPlugin> {
    let transactions = plugins_directory(storage_root).join(".transactions");
    if staged_path.parent() != Some(transactions.as_path()) {
        bail!("插件临时目录不在受管事务目录中: {}", staged_path.display());
    }
    ensure_directory(staged_path)?;
    let manifest = PluginManifest::load(&staged_path.join(MANIFEST_FILE))?;
    let signed_release = verify_signed_release(staged_path, &manifest)?;
    manifest.validate_ui_native_sandbox(signed_release.is_some())?;
    let installed = InstalledPlugin {
        directory: staged_path.to_path_buf(),
        manifest,
        enabled: true,
        signed_release,
    };
    let result = (|| {
        let wasm_bytes = match installed.manifest.wasm_binary() {
            Some(_) => Some(Arc::new(read_wasm_bytes(&installed)?)),
            None => None,
        };
        let sidecar = resolve_sidecar(storage_root, &installed, true)?;
        if let Some(sidecar_manifest) = &installed.manifest.sidecar {
            let binary = sidecar_binary_path(staged_path, &sidecar_manifest.binary)?;
            if !binary.is_file() {
                bail!("插件 sidecar 制品不存在: {}", binary.display());
            }
        }
        compile_plugin(
            wasm_bytes.clone().unwrap_or_else(|| Arc::new(Vec::new())),
            &installed.manifest,
            sidecar,
            PluginRuntimeConfig::default(),
        )?;
        let _ = wasm_bytes;
        Ok(())
    })();
    remove_sidecar_connection(staged_path);
    result?;
    Ok(installed)
}

fn install_new_plugin(
    storage_root: &Path,
    staged_path: &Path,
    manifest: PluginManifest,
) -> Result<PluginStatus> {
    let destination = plugin_directory(storage_root, &manifest.id);
    // 目录已存在但插件未被注册（如签名校验失败被忽略的旧版残留）：
    // 按升级路径原子切换并保留数据目录，而不是拒绝导入。
    if destination.exists() {
        match PluginManifest::load(&destination.join(MANIFEST_FILE)) {
            Ok(existing_manifest) => {
                let existing = InstalledPlugin {
                    directory: destination.clone(),
                    manifest: existing_manifest,
                    enabled: true,
                    signed_release: None,
                };
                return replace_installed_plugin(storage_root, staged_path, &existing, manifest);
            }
            Err(_) => {
                // 无法解析的坏残留：挪进事务目录丢弃后全新安装
                let discard = transaction_directory(storage_root, "discard-stale")?;
                std::fs::rename(&destination, &discard)?;
            }
        }
    }
    let retained = if destination.exists() {
        validate_retained_data_directory(&destination)?;
        let retained = transaction_directory(storage_root, "retained-data")?;
        std::fs::rename(&destination, &retained)?;
        if let Err(error) = move_entry(&retained, staged_path, "data") {
            let _ = std::fs::rename(&retained, &destination);
            return Err(error);
        }
        Some(retained)
    } else {
        None
    };

    if let Err(error) = std::fs::rename(staged_path, &destination) {
        if let Some(retained) = &retained {
            let _ = move_entry(staged_path, retained, "data");
            let _ = std::fs::rename(retained, &destination);
        }
        return Err(error).with_context(|| format!("安装插件 {} 失败", manifest.id));
    }

    let installed = InstalledPlugin {
        directory: destination.clone(),
        manifest: manifest.clone(),
        enabled: true,
        signed_release: verify_signed_release(&destination, &manifest)?,
    };
    let loaded = load_plugin_record(storage_root, installed);
    if (loaded.ui_plugin.is_none() && manifest.wasm_binary().is_some())
        || loaded.last_error.is_some()
    {
        let error = loaded
            .last_error
            .unwrap_or_else(|| "WASM 插件加载失败".to_string());
        let _ = stop_connection_for_directory(&destination);
        std::fs::rename(&destination, staged_path)?;
        if let Some(retained) = &retained {
            move_entry(staged_path, retained, "data")?;
            std::fs::rename(retained, &destination)?;
        }
        bail!("安装插件 {} 失败: {error}", manifest.id);
    }
    loaded_plugins()
        .lock()
        .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?
        .insert(manifest.id.clone(), loaded);
    if let Some(retained) = retained
        && let Err(error) = remove_directory_if_exists(&retained)
    {
        tracing::warn!(path = %retained.display(), %error, "插件已安装，但清理数据迁移目录失败");
    }
    list_plugin_status_without_preload(&manifest)
        .ok_or_else(|| anyhow::anyhow!("插件 {} 安装后状态丢失", manifest.id))
}

fn replace_installed_plugin(
    storage_root: &Path,
    staged_path: &Path,
    current: &InstalledPlugin,
    manifest: PluginManifest,
) -> Result<PluginStatus> {
    stop_loaded_sidecar(&current.manifest.id)?;
    // 升级同样补齐连接表兜底，确保旧 sidecar 进程被停止后再替换二进制文件，
    // 避免 Windows 上旧进程占用导致目录切换或旧文件清理失败。
    stop_connection_for_directory(&current.directory)?;
    kill_sidecar_orphans(current);
    crate::ts_tools::cancel_plugin_calls(&current.manifest.id);
    unload_plugin_wasm(&current.manifest.id);
    let rollback = rollback_directory(&current.directory, &current.manifest.id);
    if let Some(parent) = rollback.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let saved_rollback = if rollback.exists() {
        let saved = transaction_directory(storage_root, "previous-rollback")?;
        std::fs::rename(&rollback, &saved)?;
        Some(saved)
    } else {
        None
    };

    let switch_result = (|| {
        rename_with_retry(&current.directory, &rollback)?;
        move_preserved_entries(&rollback, staged_path)?;
        set_disabled_marker(staged_path, !current.enabled)?;
        rename_with_retry(staged_path, &current.directory)?;
        Ok::<_, anyhow::Error>(())
    })();
    if let Err(error) = switch_result {
        tracing::error!(plugin_id = %current.manifest.id, %error, "切换插件目录失败，尝试恢复旧版本");
        let _ = restore_upgrade_directories(staged_path, &current.directory, &rollback);
        let _ = restore_saved_rollback(&rollback, saved_rollback.as_deref());
        let _ = reload_plugin_inner(storage_root, current);
        return Err(error).with_context(|| format!("切换插件 {} 目录失败", current.manifest.id));
    }

    let upgraded = InstalledPlugin {
        directory: current.directory.clone(),
        manifest: manifest.clone(),
        enabled: current.enabled,
        signed_release: verify_signed_release(&current.directory, &manifest)?,
    };
    if let Err(error) = reload_plugin_inner(storage_root, &upgraded) {
        tracing::error!(plugin_id = %current.manifest.id, %error, "升级后重新加载插件失败，尝试恢复旧版本");
        let _ = stop_connection_for_directory(&current.directory);
        let restore_result =
            restore_upgrade_directories(staged_path, &current.directory, &rollback)
                .and_then(|()| restore_saved_rollback(&rollback, saved_rollback.as_deref()))
                .and_then(|()| reload_plugin_inner(storage_root, current));
        if let Err(restore_error) = restore_result {
            bail!(
                "升级插件 {} 失败: {error}; 恢复旧版本失败: {restore_error}",
                current.manifest.id
            );
        }
        set_last_error(&current.manifest.id, error.to_string());
        return Err(error).with_context(|| format!("升级插件 {} 失败", current.manifest.id));
    }
    if let Some(saved) = saved_rollback
        && let Err(error) = remove_directory_if_exists(&saved)
    {
        tracing::warn!(path = %saved.display(), %error, "插件已升级，但清理旧回滚目录失败");
    }
    list_plugin_status_without_preload(&manifest)
        .ok_or_else(|| anyhow::anyhow!("插件 {} 升级后状态丢失", manifest.id))
}

fn restore_upgrade_directories(staged: &Path, destination: &Path, rollback: &Path) -> Result<()> {
    if destination.exists() {
        rename_with_retry(destination, staged)?;
    }
    if PRESERVED_ENTRIES
        .iter()
        .any(|entry| staged.join(entry).exists())
    {
        move_preserved_entries(staged, rollback)?;
    }
    if rollback.exists() {
        rename_with_retry(rollback, destination)?;
    }
    Ok(())
}

fn restore_saved_rollback(rollback: &Path, saved: Option<&Path>) -> Result<()> {
    if let Some(saved) = saved
        && saved.exists()
    {
        std::fs::rename(saved, rollback)?;
    }
    Ok(())
}

fn swap_with_rollback(
    destination: &Path,
    rollback: &Path,
    transaction: &Path,
    enabled: bool,
) -> Result<()> {
    std::fs::rename(destination, transaction)?;
    if let Err(error) = std::fs::rename(rollback, destination) {
        let _ = std::fs::rename(transaction, destination);
        return Err(error.into());
    }
    if let Err(error) = move_preserved_entries(transaction, destination) {
        let _ = std::fs::rename(destination, rollback);
        let _ = std::fs::rename(transaction, destination);
        return Err(error);
    }
    if let Err(error) = set_disabled_marker(destination, !enabled) {
        let _ = move_preserved_entries(destination, transaction);
        let _ = std::fs::rename(destination, rollback);
        let _ = std::fs::rename(transaction, destination);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(transaction, rollback) {
        let _ = move_preserved_entries(destination, transaction);
        let _ = std::fs::rename(destination, rollback);
        let _ = std::fs::rename(transaction, destination);
        return Err(error.into());
    }
    Ok(())
}

fn preserve_only_data(source: &Path, destination: &Path) -> Result<()> {
    let data = source.join("data");
    std::fs::create_dir_all(destination)?;
    if data.exists() {
        std::fs::rename(&data, destination.join("data"))?;
    } else {
        std::fs::create_dir_all(destination.join("data"))?;
    }
    if let Err(error) = remove_directory_if_exists(source) {
        tracing::warn!(path = %source.display(), %error, "插件已卸载并保留数据，但清理旧制品失败");
    }
    Ok(())
}

fn validate_retained_data_directory(path: &Path) -> Result<()> {
    ensure_directory(path)?;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name() != "data" || !entry.file_type()?.is_dir() {
            bail!("插件目录已存在且不是可恢复的数据目录: {}", path.display());
        }
    }
    Ok(())
}

fn ensure_installable_version(
    current: &PluginManifest,
    next: &PluginManifest,
    allow_same_version: bool,
) -> Result<()> {
    let current_version = Version::parse(&current.version)
        .with_context(|| format!("当前插件 {} 版本无效", current.id))?;
    let next_version =
        Version::parse(&next.version).with_context(|| format!("插件 {} 新版本无效", next.id))?;
    if next_version < current_version {
        bail!(
            "插件 {} 导入版本 {} 低于当前版本 {}",
            current.id,
            next.version,
            current.version
        );
    }
    if !allow_same_version && next_version == current_version {
        bail!(
            "插件 {} 可安装版本 {} 不高于当前版本 {}",
            current.id,
            next.version,
            current.version
        );
    }
    Ok(())
}

fn move_preserved_entries(source: &Path, destination: &Path) -> Result<()> {
    for entry in PRESERVED_ENTRIES {
        remove_directory_if_exists(&destination.join(entry))?;
    }

    let mut moved = Vec::new();
    for entry in PRESERVED_ENTRIES {
        let source_entry = source.join(entry);
        if !source_entry.exists() {
            continue;
        }
        let destination_entry = destination.join(entry);
        if let Err(error) = rename_with_retry(&source_entry, &destination_entry) {
            for moved_entry in moved.into_iter().rev() {
                let _ = std::fs::rename(destination.join(moved_entry), source.join(moved_entry));
            }
            return Err(error);
        }
        moved.push(entry);
    }
    Ok(())
}

fn move_entry(source: &Path, destination: &Path, entry: &str) -> Result<()> {
    let source = source.join(entry);
    if !source.exists() {
        return Ok(());
    }
    let destination = destination.join(entry);
    remove_directory_if_exists(&destination)?;
    std::fs::rename(&source, &destination).with_context(|| {
        format!(
            "迁移插件目录失败: {} -> {}",
            source.display(),
            destination.display()
        )
    })
}

fn set_disabled_marker(directory: &Path, disabled: bool) -> Result<()> {
    let marker = directory.join(DISABLED_MARKER);
    if disabled {
        create_disabled_marker(&marker)
    } else {
        remove_file_if_exists(&marker)
    }
}

fn create_disabled_marker(path: &Path) -> Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("创建插件停用标记失败: {}", path.display()))
        }
    }
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("删除文件失败: {}", path.display())),
    }
}

fn ensure_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("读取目录失败: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("路径不是受管目录: {}", path.display());
    }
    Ok(())
}

/// 判断 IO 错误是否源于文件被占用。
///
/// Windows 上停止 sidecar 进程后，其二进制 image 的文件锁释放可能滞后于进程退出，
/// 紧随其后的目录改名/删除会撞上占用。ACCESS_DENIED(5) 与 SHARING_VIOLATION(32)
/// 即此类暂时性占用；其它平台极少出现，保留判断以备用。
fn is_file_locked(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5) | Some(32))
}

/// 对暂时性文件占用做有限重试的 IO 包装。
///
/// 这类占用通常在进程退出后数百毫秒内自行释放，故按固定间隔重试至超时；
/// 非占用错误立即向上抛出，避免无谓等待。
fn retry_io<F>(mut operation: F) -> Result<()>
where
    F: FnMut() -> std::io::Result<()>,
{
    const MAX_WAIT: Duration = Duration::from_secs(8);
    const INTERVAL: Duration = Duration::from_millis(100);
    let deadline = Instant::now() + MAX_WAIT;
    let mut warned = false;
    loop {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) if is_file_locked(&error) && Instant::now() < deadline => {
                if !warned {
                    warned = true;
                    tracing::warn!(
                        code = error.raw_os_error(),
                        "文件被占用，开始等待重试（进程退出后句柄或杀毒扫描释放可能有延迟）"
                    );
                }
                std::thread::sleep(INTERVAL);
            }
            Err(error) => return Err(anyhow::Error::from(error)),
        }
    }
}

/// 重命名文件/目录，遇到暂时性占用时重试。
fn rename_with_retry(from: &Path, to: &Path) -> Result<()> {
    retry_io(|| std::fs::rename(from, to))
        .with_context(|| format!("重命名失败: {} -> {}", from.display(), to.display()))
}

fn remove_directory_if_exists(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("拒绝删除符号链接目录: {}", path.display())
        }
        Ok(_) => retry_io(|| std::fs::remove_dir_all(path))
            .with_context(|| format!("删除目录失败: {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("读取目录失败: {}", path.display())),
    }
}

fn transaction_directory(storage_root: &Path, label: &str) -> Result<PathBuf> {
    let root = plugins_directory(storage_root).join(".transactions");
    std::fs::create_dir_all(&root)?;
    Ok(root.join(format!("{}-{label}", scru128::new())))
}

fn plugins_directory(storage_root: &Path) -> PathBuf {
    storage_root.join("plugins")
}

fn plugin_directory(storage_root: &Path, plugin_id: &str) -> PathBuf {
    plugins_directory(storage_root).join(plugin_id)
}

fn rollback_directory(directory: &Path, plugin_id: &str) -> PathBuf {
    directory
        .parent()
        .unwrap_or(directory)
        .join(ROLLBACK_DIR)
        .join(plugin_id)
}

fn sidecar_binary_path(directory: &Path, binary: &Path) -> Result<PathBuf> {
    let mut path = directory.join(binary);
    if !std::env::consts::EXE_SUFFIX.is_empty() {
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("sidecar 文件名无效"))?;
        if !file_name.ends_with(std::env::consts::EXE_SUFFIX) {
            path.set_file_name(format!("{file_name}{}", std::env::consts::EXE_SUFFIX));
        }
    }
    Ok(path)
}

fn stop_loaded_sidecar(plugin_id: &str) -> Result<()> {
    let sidecar = loaded_plugins()
        .lock()
        .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?
        .get(plugin_id)
        .and_then(|loaded| loaded.sidecar.clone());
    if let Some(sidecar) = sidecar {
        sidecar.stop()?;
    }
    Ok(())
}

fn stop_connection_for_directory(directory: &Path) -> Result<()> {
    let connection = sidecar_connections()
        .lock()
        .map_err(|_| anyhow::anyhow!("插件 sidecar 连接表已损坏"))?
        .get(directory)
        .cloned();
    if let Some(connection) = connection {
        connection.stop()?;
    }
    remove_sidecar_connection(directory);
    Ok(())
}

/// 兜底按二进制 image 名清理该插件的所有残留 sidecar 进程。
///
/// 注册表里的连接未必覆盖全部进程——热加载覆盖连接时旧 sidecar 进程可能成为
/// 孤儿，持续占用二进制文件。升级/卸载改写二进制前按 image 名再清一遍，避免
/// 目录改名/删除因文件被占用而失败。
fn kill_sidecar_orphans(installed: &InstalledPlugin) {
    let Some(sidecar) = installed.manifest.sidecar.as_ref() else {
        return;
    };
    let binary = sidecar_binary_path(&installed.directory, &sidecar.binary)
        .unwrap_or_else(|_| installed.directory.join(&sidecar.binary));
    crate::sidecar::kill_sidecar_processes_by_image(&binary);
}

/// 卸载插件的 WASM 实例（drop Store），释放其对安装目录的 WASI preopen 句柄。
///
/// Windows 上 cap-std 打开目录不带 FILE_SHARE_DELETE，已加载实例的 Store 会长期持有
/// 安装目录句柄，阻止其被 rename/delete，导致升级切换、卸载删除失败（code=32）。
/// 在改写目录前调用本函数清空实例，让目录可被改写；后续 reload 会重建实例。
fn unload_plugin_wasm(plugin_id: &str) {
    let adapters = {
        let mut plugins = match loaded_plugins().lock() {
            Ok(plugins) => plugins,
            Err(error) => {
                tracing::error!(plugin_id, %error, "插件注册表已损坏，无法卸载 WASM 实例");
                return;
            }
        };
        let Some(loaded) = plugins.get_mut(plugin_id) else {
            return;
        };
        loaded.ui_plugin = None;
        loaded.component = None;
        loaded.wasm_bytes = None;
        loaded
            .instances
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>()
    };
    for adapter in adapters {
        adapter.release_inner();
    }
}

fn remove_sidecar_connection(directory: &Path) {
    if let Ok(mut connections) = sidecar_connections().lock() {
        connections.remove(directory);
    }
}

/// 扫描插件目录，返回可加载插件与被忽略的无效插件。
///
/// 无效插件不再静默丢弃：签名无效、沙箱声明越权、清单损坏的目录都会登记
/// （含原因），随 `list_plugins` 展示，并支持经 `uninstall_plugin` 清理。
fn discover_installed_plugins(
    storage_root: &Path,
) -> (Vec<InstalledPlugin>, Vec<InvalidPluginEntry>) {
    let plugins_dir = storage_root.join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins_dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut manifest_paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(MANIFEST_FILE))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifest_paths.sort();

    let mut installed_plugins = Vec::new();
    let mut invalid_plugins = Vec::new();
    for path in manifest_paths {
        let Some(directory) = path.parent().map(Path::to_path_buf) else {
            continue;
        };
        let directory_name = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        match PluginManifest::load(&path) {
            Ok(manifest) => {
                let signed_release = match verify_signed_release(&directory, &manifest) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "忽略签名无效的插件");
                        invalid_plugins.push(InvalidPluginEntry {
                            id: directory_name,
                            name: manifest.id.clone(),
                            manifest_version: Some(manifest.version.clone()),
                            reason: error.to_string(),
                        });
                        continue;
                    }
                };
                if let Err(error) = manifest.validate_ui_native_sandbox(signed_release.is_some()) {
                    tracing::warn!(path = %path.display(), %error, "忽略沙箱声明越权的插件");
                    invalid_plugins.push(InvalidPluginEntry {
                        id: directory_name,
                        name: manifest.id.clone(),
                        manifest_version: Some(manifest.version.clone()),
                        reason: error.to_string(),
                    });
                    continue;
                }
                installed_plugins.push(InstalledPlugin {
                    enabled: !directory.join(DISABLED_MARKER).is_file(),
                    directory,
                    manifest,
                    signed_release,
                });
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %format!("{error:#}"), "忽略无效插件清单");
                invalid_plugins.push(InvalidPluginEntry {
                    id: directory_name.clone(),
                    // 清单读不出来时用目录名作展示名。
                    name: directory_name,
                    manifest_version: None,
                    reason: error.to_string(),
                });
            }
        }
    }
    (installed_plugins, invalid_plugins)
}

fn find_installed_plugin(storage_root: &Path, plugin_id: &str) -> Result<InstalledPlugin> {
    if plugin_id.is_empty()
        || !plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || plugin_id == "."
        || plugin_id == ".."
    {
        bail!("插件 ID 无效: {plugin_id}");
    }

    // 单插件操作直接读取目标目录，不能扫描并验签全部插件。
    let directory = plugin_directory(storage_root, plugin_id);
    let directory_metadata = std::fs::symlink_metadata(&directory)
        .with_context(|| format!("插件未安装: {plugin_id}"))?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        bail!("插件安装路径必须是实际目录: {}", directory.display());
    }
    let manifest_path = directory.join(MANIFEST_FILE);
    let manifest_metadata = std::fs::symlink_metadata(&manifest_path)
        .with_context(|| format!("插件未安装: {plugin_id}"))?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        bail!("插件清单必须是实际文件: {}", manifest_path.display());
    }
    let manifest = PluginManifest::load(&manifest_path)?;
    if manifest.id != plugin_id {
        bail!(
            "插件安装目录与清单 ID 不一致: expected={plugin_id}, actual={}",
            manifest.id
        );
    }
    let signed_release = verify_signed_release(&directory, &manifest)?;
    manifest.validate_ui_native_sandbox(signed_release.is_some())?;
    Ok(InstalledPlugin {
        enabled: !directory.join(DISABLED_MARKER).is_file(),
        directory,
        manifest,
        signed_release,
    })
}

fn resolve_sidecar(
    storage_root: &Path,
    installed: &InstalledPlugin,
    refresh: bool,
) -> Result<Option<Arc<dyn SidecarConnection>>> {
    if installed.manifest.sidecar.is_none() {
        return Ok(None);
    }
    sidecar_connection(storage_root, installed, refresh).map(Some)
}

fn sidecar_connection(
    storage_root: &Path,
    installed: &InstalledPlugin,
    refresh: bool,
) -> Result<Arc<dyn SidecarConnection>> {
    let sidecar = installed
        .manifest
        .sidecar
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("插件 {} 未声明 sidecar", installed.manifest.id))?;
    // 通道判定（RFC 0017 §5）：L1 官方签名走原路径；L4 放开开关审计放行；
    // L3 本地确认按内容哈希命中放行。其余维持"未签名不允许启动原生 sidecar"。
    let safety = tiangong_sandbox::PluginSafetyStore::open(storage_root);
    let signed_release = installed.signed_release.as_ref();
    if signed_release.is_none() {
        if safety.unsafe_mode() {
            tracing::warn!(
                plugin_id = %installed.manifest.id,
                "L4 放开开关生效：未签名插件启动原生 sidecar"
            );
        } else if safety.is_trusted(&installed.manifest.id, &installed.directory) {
            tracing::warn!(
                plugin_id = %installed.manifest.id,
                "L3 本地信任命中：未签名插件启动原生 sidecar"
            );
        } else {
            anyhow::bail!(
                "未签名插件 {} 不允许启动原生 sidecar（可经本地信任或放开开关授权）",
                installed.manifest.id
            );
        }
    }
    if let Some(signed_release) = signed_release
        && !signed_release.has_permission("sidecar.invoke")
    {
        bail!(
            "插件 {} 的官方签名未授权 sidecar.invoke",
            installed.manifest.id
        );
    }
    if !installed.manifest.permissions.is_empty()
        && !installed.manifest.has_permission("sidecar.invoke")
    {
        bail!("插件 {} 未声明 sidecar.invoke 权限", installed.manifest.id);
    }
    // 沙箱策略由宿主权威策略表决定（RFC 0017 透明执行封套）：
    // 不读 manifest 的 sandbox / sandbox_network（插件自声明是提权通道）。
    let official_signed = signed_release.is_some();
    let host_policy =
        tiangong_sandbox::host_policy::resolve(&installed.manifest.id, official_signed);
    if host_policy.sandbox && sidecar.transport != TRANSPORT_STDIO {
        bail!(
            "插件 {} 按宿主策略需进 OS 沙箱，但未使用 stdio 传输",
            installed.manifest.id
        );
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
    let (server_url, server_token) = current_server_endpoint()
        .map(|(url, token)| (Some(url), token))
        .unwrap_or((None, None));
    let config = SidecarConfig::new(
        &installed.manifest.id,
        &installed.manifest.version,
        binary,
        endpoint,
        log,
        data_dir,
        storage_root,
    )
    .with_sensitive_storage(signed_release.is_some_and(|release| {
        release.has_permission("model-config.read") || release.has_permission("app-storage.read")
    }))
    .with_protocols(&sidecar.transport_protocol, sidecar.business_protocol)
    .with_timeouts(
        Duration::from_millis(sidecar.startup_timeout_ms),
        Duration::from_millis(sidecar.request_timeout_ms),
    )
    .with_server_endpoint(server_url, server_token)
    .with_sandbox(host_policy.sandbox)
    .with_sandbox_network(host_policy.allow_network);

    let mut connections = sidecar_connections()
        .lock()
        .map_err(|_| anyhow::anyhow!("插件 sidecar 连接表已损坏"))?;
    if refresh || !connections.contains_key(&installed.directory) {
        let connection: Arc<dyn SidecarConnection> = if sidecar.transport == TRANSPORT_STDIO {
            Arc::new(StdioSidecarConnection::new(config))
        } else {
            Arc::new(crate::sidecar::ProcessSidecarConnection::new(config))
        };
        connections.insert(installed.directory.clone(), connection);
    }
    connections
        .get(&installed.directory)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("创建插件 sidecar 连接失败"))
}

fn loaded_plugin_matches(installed: &InstalledPlugin) -> Result<bool> {
    // 纯 UI/TS 插件无 WASM 字节可比较，完整清单也必须参与判断，
    // 否则工具、提示词或 UI 贡献变化不会触发热更新。
    let bytes = installed
        .manifest
        .wasm_binary()
        .map(|_| read_wasm_bytes(installed))
        .transpose()?;
    let plugins = loaded_plugins()
        .lock()
        .map_err(|_| anyhow::anyhow!("插件注册表已损坏"))?;
    let Some(loaded) = plugins.get(&installed.manifest.id) else {
        return Ok(false);
    };
    let bytes_match = match (&loaded.wasm_bytes, &bytes) {
        (Some(loaded_bytes), Some(bytes)) => loaded_bytes.as_slice() == bytes.as_slice(),
        (None, None) => true,
        _ => false,
    };
    let manifest_match =
        serde_json::to_value(&loaded.manifest)? == serde_json::to_value(&installed.manifest)?;
    Ok(loaded.directory == installed.directory
        && loaded.enabled == installed.enabled
        && manifest_match
        && bytes_match)
}
