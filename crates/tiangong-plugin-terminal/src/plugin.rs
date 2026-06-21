//! 终端进程内插件（issue #156 自注册架构）。
//!
//! [`TerminalPlugin`] 封装终端的全部能力（终端 provider + 工具覆盖 + Prompt 段落），
//! 在 engine 创建/重建时自行注册，替代 main.rs 的手工胶水代码。

use std::sync::Arc;

use tauri::{Manager, Wry};
use tiangong_core::core::Plugin;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::terminal_trait::TerminalProvider;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler};

use crate::handler::{TerminalPromptSectionProvider, TerminalToolOverride};
use crate::session_pty::SessionAwareTerminalProvider;

/// 终端插件：聚合终端能力、工具覆盖处理器与 Prompt 段落提供者，自行向 engine 注册。
pub struct TerminalPlugin {
    provider: Arc<dyn TerminalProvider>,
    override_handler: Arc<dyn ToolOverrideHandler>,
    prompt_provider: Arc<dyn PromptSectionProvider>,
}

impl TerminalPlugin {
    /// 从 Tauri 应用句柄构造终端插件。
    ///
    /// 复用现有的 `SessionAwareTerminalProvider` / `TerminalToolOverride` /
    /// `TerminalPromptSectionProvider`，仅在外层包一层「自注册」入口。
    /// 返回 `None` 表示插件 state 未就绪（与旧 `get_*` 工厂一致）。
    pub fn from_app_handle(app: &tauri::AppHandle<Wry>) -> Option<Self> {
        let state = app.state::<crate::TerminalPluginState>();
        let provider: Arc<dyn TerminalProvider> =
            Arc::new(SessionAwareTerminalProvider::new(state.registry.clone()));
        let override_handler: Arc<dyn ToolOverrideHandler> =
            Arc::new(TerminalToolOverride::new(provider.clone()));
        let prompt_provider: Arc<dyn PromptSectionProvider> =
            Arc::new(TerminalPromptSectionProvider);
        Some(Self {
            provider,
            override_handler,
            prompt_provider,
        })
    }
}

impl Plugin for TerminalPlugin {
    fn id(&self) -> &str {
        "terminal"
    }

    fn register(&self, engine: &RuntimeEngine) {
        engine.set_terminal_provider(self.provider.clone());
        // 终端覆盖的工具（与旧 main.rs 手工注册清单一致）
        engine.register_tool_override("run_shell", self.override_handler.clone());
        engine.register_tool_override("terminal_send", self.override_handler.clone());
        engine.register_prompt_section_provider(self.prompt_provider.clone());
    }
}
