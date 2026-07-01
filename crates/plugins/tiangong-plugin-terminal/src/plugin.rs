//! 终端进程内插件（issue #156 自注册架构）。
//!
//! [`TerminalPlugin`] 封装终端的全部能力（终端 provider + 工具覆盖 + Prompt 段落），
//! 在 engine 创建/重建时自行注册，替代 main.rs 的手工胶水代码。
//!
//! 工具规格（run_shell / terminal_send）与覆盖处理器、Prompt 段落直接在
//! [`TerminalPlugin`] 上实现，core 通过 supertrait 自动收集。

use std::sync::Arc;

use serde_json::json;
use tauri::{Manager, Wry};
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::terminal_trait::TerminalProvider;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler};

use crate::handler::{TerminalPromptSectionProvider, TerminalToolOverride};
use crate::session_pty::SessionAwareTerminalProvider;

/// 终端插件：聚合终端能力、工具覆盖处理器与 Prompt 段落提供者。
pub struct TerminalPlugin {
    provider: Arc<dyn TerminalProvider>,
    override_handler: TerminalToolOverride,
    prompt_provider: TerminalPromptSectionProvider,
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
        let override_handler = TerminalToolOverride::new(provider.clone());
        let prompt_provider = TerminalPromptSectionProvider;
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

    fn register(&self, engine: &tiangong_core::runtime::RuntimeEngine) {
        // 注入终端能力（校验通过后可通过 PTY 执行命令）
        engine.set_terminal_provider(self.provider.clone());
        // 工具规格 / 工具覆盖 / Prompt 段落由 core 通过 supertrait 自动收集，
        // 此处仅注入 TerminalProvider 能力。
    }
}

impl ToolOverrideHandler for TerminalPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        ToolOverrideHandler::handle(&self.override_handler, call, session_id)
    }
}

impl tiangong_core::tool_override::ToolSpecProvider for TerminalPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        // 终端覆盖的工具规格：执行全部路由到本插件的 handle。
        // 必须由本插件提供 spec（core 才能按 spec.name 注册 override）。
        vec![
            ToolSpec {
                name: "run_shell".to_string(),
                description: "在终端执行 shell 脚本（通过 PTY，支持交互式程序）。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "script": { "type": "string", "description": "要执行的 shell 脚本" },
                        "cwd": { "type": "string", "description": "工作目录（可选）" },
                        "timeout": { "type": "integer", "description": "超时秒数（可选）" },
                        "interactive": { "type": "boolean", "description": "是否启动交互程序，默认 false" }
                    },
                    "required": ["script"]
                }),
            },
            ToolSpec {
                name: "terminal_send".to_string(),
                description: "向终端发送按键/文本输入。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "input": { "type": "string", "description": "要发送的按键/文本" },
                        "wait": { "type": "integer", "description": "发送后等待秒数，默认 3", "minimum": 1 }
                    },
                    "required": ["input"]
                }),
            },
        ]
    }
}

impl PromptSectionProvider for TerminalPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        PromptSectionProvider::prompt_sections(&self.prompt_provider)
    }
}
