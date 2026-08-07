//! 桌面端 Core 构造依赖（issue #245）。
//!
//! 原 `TiangongApp::create_core` 的插件构造 body 收敛至此。`CoreManager` 内置
//! TiangongCore 构造（针对该类型，不抽象），host 在调用 `ensure_core` 前先经
//! [`DesktopCoreFactory::build_plugins`] 构造好插件集合并作为参数传入。
//!
//! host 专属状态（Tauri app_handle、CoreConfigProvider 的 generation）留在本结构。
//! skill/mcp 等 WASM 插件由 `load_installed_plugins` 自动加载，不再手动注入。

use std::sync::Arc;

use tauri::AppHandle;
use tiangong_core::core::Plugin;
use tiangong_core::core_config::CoreConfigProvider;

/// 桌面端 Core 构造依赖。
///
/// 与 `TiangongApp` 共享以下句柄（同一实例，dual-ownership 语义不变）：
/// - `app_handle`：Tauri 句柄（setup 阶段注入；构造时尚未就绪）
/// - `config`：全局 CoreConfigProvider
/// - `storage_root`：会话文件根
#[derive(Clone)]
pub struct DesktopCoreFactory {
    pub app_handle: Arc<std::sync::OnceLock<AppHandle>>,
    pub config: CoreConfigProvider,
    pub storage_root: std::path::PathBuf,
}

impl DesktopCoreFactory {
    /// 构造桌面端 Core 所需的完整插件集合（issue #245）。
    ///
    /// 调用方（`TiangongApp`）在 `ensure_core` 前调用本方法，把返回的 plugins
    /// 作为参数传给 `CoreManager::ensure_core`。Core 的实际 builder 构造由
    /// CoreManager 内部完成，host 不再直接 build TiangongCore。
    /// 构造桌面端 Core 所需的完整插件集合（issue #245）。
    ///
    /// 同步函数：body 内无异步操作，但保留 `async` 签名仅为历史兼容。调用方经
    /// `CoreManager::ensure_core` 的按需回调传入，只有 Core 不存在时才会执行。
    pub fn build_plugins_sync(
        &self,
        _models: tiangong_llm::models_config::ModelsConfig,
    ) -> Vec<Arc<dyn Plugin>> {
        use tracing::{info, warn};

        let storage_root = self.storage_root.clone();
        let mut plugins: Vec<Arc<dyn Plugin>> = Vec::new();
        // 产品文案插件注册在最前，保证身份/规则段排在 system prompt 开头。
        let Some(app_handle) = self.app_handle.get().cloned() else {
            warn!("app_handle 尚未注入，浏览器/终端能力将缺失");
            return plugins;
        };
        let browser_available =
            if let Some(browser) = tiangong_plugin_browser::build_plugin(&app_handle) {
                plugins.push(browser);
                true
            } else {
                warn!("浏览器插件构造失败（Tauri state 未就绪），浏览器能力将缺失");
                false
            };
        let terminal_available =
            if let Some(terminal) = tiangong_plugin_terminal::build_plugin(&app_handle) {
                plugins.push(terminal);
                true
            } else {
                warn!("终端插件构造失败（Tauri state 未就绪），终端能力将缺失");
                false
            };
        // 所有媒体插件（image/video/tts/stt/attachment）由 load_installed_plugins 自动加载。
        // fs（基础文件工具）也由 load_installed_plugins 自动加载（issue #330）。
        let wasm_plugins = tiangong_plugin_runtime::registry::load_installed_plugins(
            &self.storage_root,
            tiangong_plugin_runtime::registry::RuntimeKind::Desktop,
        );
        info!(count = wasm_plugins.len(), "已加载 WASM 插件");
        plugins.extend(wasm_plugins);
        plugins.push(tiangong_plugin_task::build_plugin());
        // skill/analyze-attachment 等 WASM 插件由上面的 load_installed_plugins 自动加载。
        // Agent Team 插件：子 Agent 管理 + 文件锁工具（issue #200）。
        let child_plugin_factory = Arc::new({
            let app_handle = app_handle.clone();
            let storage_root = storage_root.clone();
            move || {
                let mut child_plugins: Vec<Arc<dyn Plugin>> = Vec::new();
                if browser_available {
                    if let Some(browser) = tiangong_plugin_browser::build_plugin(&app_handle) {
                        child_plugins.push(browser);
                    }
                }
                if terminal_available {
                    if let Some(terminal) = tiangong_plugin_terminal::build_plugin(&app_handle) {
                        child_plugins.push(terminal);
                    }
                }
                child_plugins.extend(tiangong_plugin_runtime::registry::load_installed_plugins(
                    &storage_root,
                    tiangong_plugin_runtime::registry::RuntimeKind::Desktop,
                ));
                child_plugins.push(tiangong_plugin_task::build_plugin());
                child_plugins
            }
        });
        plugins.push(tiangong_plugin_agent_team::build_plugin(
            storage_root.clone(),
            child_plugin_factory,
        ));
        plugins
    }
}
