//! 桌面端 Core 构造工厂（issue #245）。
//!
//! 把原 `TiangongApp::create_core` 的构造 body 收敛至此处，作为 `CoreManager`
//! 注入的 `CoreFactory` 实现。host 专属状态（Tauri app_handle、skill/mcp 管理
//! 插件、CoreConfigProvider 的 generation）全部留在本结构，`CoreManager` 自身
//! 保持 host 无关。

use std::sync::mpsc::Sender;
use std::sync::Arc;

use async_trait::async_trait;
use tauri::AppHandle;
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
use tiangong_core_manager::CoreFactory;
use tiangong_types::StreamEvent;

/// 桌面端 Core 工厂。
///
/// 与 `TiangongApp` 共享以下句柄（同一实例，dual-ownership 语义不变）：
/// - `app_handle`：Tauri 句柄（setup 阶段注入；构造时尚未就绪）
/// - `skill_plugin` / `mcp_plugin`：管理插件句柄（core 拿 clone 做 LLM 工具）
/// - `config`：全局 CoreConfigProvider（取 generation 用于 memory handle 初始化）
/// - `storage_root`：会话文件根
#[derive(Clone)]
pub struct DesktopCoreFactory {
    pub app_handle: Arc<std::sync::OnceLock<AppHandle>>,
    pub skill_plugin: Arc<tiangong_plugin_skill::SkillPlugin>,
    pub mcp_plugin: Arc<tiangong_plugin_mcp::McpPlugin>,
    pub config: CoreConfigProvider,
    pub storage_root: std::path::PathBuf,
}

#[async_trait]
impl CoreFactory for DesktopCoreFactory {
    async fn create(
        &self,
        session_id: &str,
        session_config: CoreConfig,
        stream_tx: Sender<StreamEvent>,
    ) -> Result<TiangongCore, String> {
        // session_id 即真相源 id，Core 自行从磁盘 load session（不再传 Session）。
        let session =
            tiangong_core::session::Session::load_from_storage(&self.storage_root, session_id)
                .map_err(|error| {
                    format!("Core 构造前加载 session 失败（{session_id}）：{error}")
                })?;

        let core = self
            .build_core(session, session_config, stream_tx)
            .await
            .map_err(|error| error.to_string())?;
        Ok(core)
    }
}

impl DesktopCoreFactory {
    /// 实际的 Core 构造 body（与原 `TiangongApp::create_core` 一致）。
    ///
    /// 拆出为独立方法便于既有 `TiangongApp::create_core` 在 P1 并存期间复用。
    async fn build_core(
        &self,
        session: tiangong_core::session::Session,
        session_config: CoreConfig,
        stream_tx: Sender<StreamEvent>,
    ) -> Result<TiangongCore, tiangong_core::core::CoreError> {
        use tracing::warn;

        let memory_handle = tiangong_memory::registry::init_memory_handle_for_process(
            self.config.generation(),
            tiangong_memory::ProcessType::Gui,
        )
        .await;

        let storage_root = self.storage_root.clone();
        let mut plugins: Vec<Arc<dyn tiangong_core::core::Plugin>> = Vec::new();
        // 产品文案插件注册在最前，保证身份/规则段排在 system prompt 开头。
        plugins.extend(tiangong_plugin_prompt::default_plugins());
        let Some(app_handle) = self.app_handle.get() else {
            return Err(tiangong_core::core::CoreError::WorkerStopped);
        };
        let browser_available =
            if let Some(browser) = tiangong_plugin_browser::build_plugin(app_handle) {
                plugins.push(browser);
                true
            } else {
                warn!("浏览器插件构造失败（Tauri state 未就绪），浏览器能力将缺失");
                false
            };
        let terminal_available =
            if let Some(terminal) = tiangong_plugin_terminal::build_plugin(app_handle) {
                plugins.push(terminal);
                true
            } else {
                warn!("终端插件构造失败（Tauri state 未就绪），终端能力将缺失");
                false
            };
        plugins.push(tiangong_plugin_fs::build_plugin());
        plugins.push(tiangong_plugin_index::build_plugin());
        // app 层判断是否注册各能力插件，经 llm 路由解析端点后构造注入。
        use tiangong_llm::{ModelCapability, ModelEndpoint, SingleProviderClient};
        let models = tiangong_config::registry::models();
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
        plugins.push(tiangong_plugin_memory::build_plugin(memory_handle.clone()));
        plugins.push(tiangong_plugin_scheduler::build_plugin());
        plugins.push(tiangong_plugin_task::build_plugin());
        if let Some(client) = multimodal_endpoint.clone().map(SingleProviderClient::new) {
            plugins.push(tiangong_plugin_analyze_attachment::build_plugin(client));
        }
        // Skill / MCP 插件：dual-ownership——core 拿 clone 做 LLM 工具，
        // app 侧经 self.skill_plugin / self.mcp_plugin 做管理。
        plugins.push(self.skill_plugin.clone());
        plugins.push(self.mcp_plugin.clone());
        // Agent Team 插件：子 Agent 管理 + 文件锁工具（issue #200）。
        let child_plugin_factory: Arc<dyn tiangong_plugin_agent_team::ChildPluginFactory> =
            Arc::new({
                let app_handle = app_handle.clone();
                let storage_root = storage_root.clone();
                move || {
                    let mut child_plugins: Vec<Arc<dyn tiangong_core::core::Plugin>> = Vec::new();
                    child_plugins.extend(tiangong_plugin_prompt::default_plugins());
                    if browser_available {
                        if let Some(browser) = tiangong_plugin_browser::build_plugin(&app_handle) {
                            child_plugins.push(browser);
                        }
                    }
                    if terminal_available {
                        if let Some(terminal) = tiangong_plugin_terminal::build_plugin(&app_handle)
                        {
                            child_plugins.push(terminal);
                        }
                    }
                    child_plugins.push(tiangong_plugin_fs::build_plugin());
                    child_plugins.push(tiangong_plugin_index::build_plugin());
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
                    child_plugins.push(tiangong_plugin_memory::build_plugin(memory_handle.clone()));
                    child_plugins.push(tiangong_plugin_scheduler::build_plugin());
                    child_plugins.push(tiangong_plugin_task::build_plugin());
                    if let Some(client) = multimodal_endpoint.clone().map(SingleProviderClient::new)
                    {
                        child_plugins
                            .push(tiangong_plugin_analyze_attachment::build_plugin(client));
                    }
                    child_plugins.push(Arc::new(
                        tiangong_plugin_skill::SkillPlugin::with_storage_root(
                            storage_root.join("skills"),
                        ),
                    ));
                    child_plugins.push(Arc::new(
                        tiangong_plugin_mcp::McpPlugin::with_storage_root(storage_root.clone()),
                    ));
                    child_plugins
                }
            });
        plugins.push(tiangong_plugin_agent_team::build_plugin(
            storage_root.clone(),
            child_plugin_factory,
        ));

        Ok(TiangongCore::builder()
            .session_id(session.id.clone())
            .config(CoreConfigProvider::new(session_config))
            .trust_mode(session.trust_mode)
            .storage_root(storage_root)
            .stream_tx(stream_tx)
            .plugins(plugins)
            .build())
    }
}
