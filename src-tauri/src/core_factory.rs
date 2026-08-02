//! 桌面端 Core 构造依赖（issue #245）。
//!
//! 原 `TiangongApp::create_core` 的插件构造 body 收敛至此。`CoreManager` 内置
//! TiangongCore 构造（针对该类型，不抽象），host 在调用 `ensure_core` 前先经
//! [`DesktopCoreFactory::build_plugins`] 构造好插件集合并作为参数传入。
//!
//! host 专属状态（Tauri app_handle、skill/mcp 管理插件、CoreConfigProvider 的
//! generation）全部留在本结构。

use std::sync::Arc;

use tauri::AppHandle;
use tiangong_core::core::Plugin;
use tiangong_core::core_config::CoreConfigProvider;
use tiangong_scheduler::executor::SchedulerContext;

/// 桌面端 Core 构造依赖。
///
/// 与 `TiangongApp` 共享以下句柄（同一实例，dual-ownership 语义不变）：
/// - `app_handle`：Tauri 句柄（setup 阶段注入；构造时尚未就绪）
/// - `skill_plugin` / `mcp_plugin`：管理插件句柄（core 拿 clone 做 LLM 工具）
/// - `config`：全局 CoreConfigProvider
/// - `storage_root`：会话文件根
/// - `scheduler_context`：调度器执行上下文（让 Agent 手动触发定时任务能真正执行）
#[derive(Clone)]
pub struct DesktopCoreFactory {
    pub app_handle: Arc<std::sync::OnceLock<AppHandle>>,
    pub skill_plugin: Arc<tiangong_plugin_skill::SkillPlugin>,
    pub config: CoreConfigProvider,
    pub storage_root: std::path::PathBuf,
    pub scheduler_context: Arc<dyn SchedulerContext>,
}

impl DesktopCoreFactory {
    /// 构造桌面端 Core 所需的完整插件集合（issue #245）。
    ///
    /// 调用方（`TiangongApp`）在 `ensure_core` 前调用本方法，把返回的 plugins
    /// 作为参数传给 `CoreManager::ensure_core`。Core 的实际 builder 构造由
    /// CoreManager 内部完成，host 不再直接 build TiangongCore。
    pub async fn build_plugins(
        &self,
        models: tiangong_llm::models_config::ModelsConfig,
    ) -> Vec<Arc<dyn Plugin>> {
        use tracing::{info, warn};

        let storage_root = self.storage_root.clone();
        let mut plugins: Vec<Arc<dyn Plugin>> = Vec::new();
        // 产品文案插件注册在最前，保证身份/规则段排在 system prompt 开头。
        plugins.extend(tiangong_plugin_prompt::default_plugins());
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
        plugins.push(tiangong_plugin_fs::build_plugin());
        // app 层判断是否注册各能力插件，经 llm 路由解析端点后构造注入。
        use tiangong_llm::{ModelCapability, ModelEndpoint, SingleProviderClient};
        let resolve_ep = |cap: ModelCapability| {
            models
                .resolve_for_capability(cap)
                .map(ModelEndpoint::from_resolved)
        };
        let image_endpoint = resolve_ep(ModelCapability::ImageGeneration);
        let video_endpoint = resolve_ep(ModelCapability::VideoGeneration);
        let tts_endpoint = resolve_ep(ModelCapability::Tts);
        let stt_endpoint = resolve_ep(ModelCapability::Stt);
        let multimodal_endpoint =
            if models.has_capability(ModelCapability::Multimodal) && !models.chat_is_multimodal() {
                resolve_ep(ModelCapability::Multimodal)
            } else {
                None
            };
        if let Some(ep) = image_endpoint.clone() {
            plugins.push(tiangong_plugin_generate_image::build_plugin(ep));
        }
        if let Some(ep) = video_endpoint.clone() {
            plugins.push(tiangong_plugin_generate_video::build_plugin(ep));
        }
        if let Some(ep) = tts_endpoint.clone() {
            plugins.push(tiangong_plugin_text_to_speech::build_plugin(ep));
        }
        if let Some(ep) = stt_endpoint.clone() {
            plugins.push(tiangong_plugin_speech_to_text::build_plugin(ep));
        }
        let wasm_plugins =
            tiangong_plugin_runtime::registry::load_installed_plugins(&self.storage_root);
        info!(count = wasm_plugins.len(), "已加载 WASM 插件");
        plugins.extend(wasm_plugins);
        // 调度器插件注入执行上下文：让 Agent 手动触发 scheduler_trigger_job 时
        // 能真正执行任务（execute_job）。
        plugins.push(tiangong_plugin_scheduler::build_plugin(
            self.scheduler_context.clone(),
        ));
        plugins.push(tiangong_plugin_task::build_plugin());
        if let Some(client) = multimodal_endpoint.clone().map(SingleProviderClient::new) {
            plugins.push(tiangong_plugin_analyze_attachment::build_plugin(client));
        }
        // Skill / MCP 插件：dual-ownership——core 拿 clone 做 LLM 工具，
        // app 侧经 self.skill_plugin 做管理。
        plugins.push(self.skill_plugin.clone());
        // Agent Team 插件：子 Agent 管理 + 文件锁工具（issue #200）。
        let child_plugin_factory = Arc::new({
            let app_handle = app_handle.clone();
            let storage_root = storage_root.clone();
            let scheduler_context = self.scheduler_context.clone();
            move || {
                let mut child_plugins: Vec<Arc<dyn Plugin>> = Vec::new();
                child_plugins.extend(tiangong_plugin_prompt::default_plugins());
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
                child_plugins.push(tiangong_plugin_fs::build_plugin());
                if let Some(ep) = image_endpoint.clone() {
                    child_plugins.push(tiangong_plugin_generate_image::build_plugin(ep));
                }
                if let Some(ep) = video_endpoint.clone() {
                    child_plugins.push(tiangong_plugin_generate_video::build_plugin(ep));
                }
                if let Some(ep) = tts_endpoint.clone() {
                    child_plugins.push(tiangong_plugin_text_to_speech::build_plugin(ep));
                }
                if let Some(ep) = stt_endpoint.clone() {
                    child_plugins.push(tiangong_plugin_speech_to_text::build_plugin(ep));
                }
                child_plugins.extend(tiangong_plugin_runtime::registry::load_installed_plugins(
                    &storage_root,
                ));
                // 子 Core（Agent Team）同样注入调度执行上下文，保持与主 Core 一致。
                child_plugins.push(tiangong_plugin_scheduler::build_plugin(
                    scheduler_context.clone(),
                ));
                child_plugins.push(tiangong_plugin_task::build_plugin());
                if let Some(client) = multimodal_endpoint.clone().map(SingleProviderClient::new) {
                    child_plugins.push(tiangong_plugin_analyze_attachment::build_plugin(client));
                }
                child_plugins.push(Arc::new(
                    tiangong_plugin_skill::SkillPlugin::with_storage_root(
                        storage_root.join("skills"),
                    ),
                ));
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
