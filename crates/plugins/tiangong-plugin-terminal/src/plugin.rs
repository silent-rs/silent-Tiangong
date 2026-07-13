//! 终端进程内插件（issue #156 自注册架构）。
//!
//! [`TerminalPlugin`] 封装终端的全部能力（终端 provider + 工具覆盖 + Prompt 段落），
//! 在 engine 创建/重建时自行注册，替代 main.rs 的手工胶水代码。
//!
//! 工具规格（run_shell / terminal_send）与覆盖处理器、Prompt 段落直接在
//! [`TerminalPlugin`] 上实现，core 通过 supertrait 自动收集。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde_json::json;
use tauri::{Manager, Wry};
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler};

use crate::capability::TerminalProvider;
use crate::handler::{TerminalPromptSectionProvider, TerminalToolOverride};
use crate::session_pty::SessionAwareTerminalProvider;
use tiangong_core::permission::TrustModeHandle;

/// 终端插件：聚合终端能力、工具覆盖处理器与 Prompt 段落提供者。
///
/// `provider` 不作为字段持有——终端能力是插件内部状态，由 `TerminalToolOverride`
/// 在构造时捕获并独占使用，插件外壳无需再保留一份（#225 能力下沉）。
pub struct TerminalPlugin {
    override_handler: TerminalToolOverride,
    prompt_provider: TerminalPromptSectionProvider,
    workspace: RwLock<Option<PathBuf>>,
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
        let override_handler = TerminalToolOverride::new(provider);
        let prompt_provider = TerminalPromptSectionProvider;
        Some(Self {
            override_handler,
            prompt_provider,
            workspace: RwLock::new(None),
        })
    }
}

impl Plugin for TerminalPlugin {
    fn id(&self) -> &str {
        "terminal"
    }

    // register 留空：终端能力是插件内部状态，由 handler 直接持有 provider 调用，
    // 不再经 RuntimeEngine 中转注入（#225 能力下沉）。

    /// 注入信任模式解析句柄：透传给 handler，FullTrust 时跳过 run_command 校验。
    fn set_trust_mode(&self, trust: TrustModeHandle) {
        self.override_handler.set_trust_mode(trust);
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(std::path::Path::to_path_buf);
        }
    }
}

impl ToolOverrideHandler for TerminalPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut tiangong_core::session::Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        // 嵌套执行单元不能沿用用户终端可能已经切换过的 cwd，否则外层按工作区
        // 声明的写入边界与真实进程目录不一致。显式切回插件收到的会话工作区。
        let workspace = self.workspace.read().ok().and_then(|guard| guard.clone());
        let normalized = normalize_nested_terminal_call(
            call,
            session.parent_session_id.is_some(),
            workspace.as_deref(),
        );
        ToolOverrideHandler::handle(
            &self.override_handler,
            normalized.as_ref().unwrap_or(call),
            session,
            actor_id,
        )
    }
}

fn normalize_nested_terminal_call(
    call: &ToolCall,
    is_child_session: bool,
    workspace: Option<&std::path::Path>,
) -> Option<ToolCall> {
    if !is_child_session || !matches!(call.name.as_str(), "run_command" | "run_shell") {
        return None;
    }
    workspace.map(|workspace| {
        let mut normalized = call.clone();
        normalized.arguments["cwd"] = serde_json::Value::String(workspace.display().to_string());
        normalized
    })
}

impl tiangong_core::tool_override::ToolSpecProvider for TerminalPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        // 终端覆盖的工具规格：执行全部路由到本插件的 handle。
        // 必须由本插件提供 spec（core 才能按 spec.name 注册 override）。
        vec![
            ToolSpec {
                name: "run_command".to_string(),
                description: "执行受控命令（通过 PTY，输出回显到终端面板）。支持 cwd 和超时设置。"
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "cmd": { "type": "string", "description": "命令名" },
                        "args": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "命令参数列表"
                        },
                        "cwd": { "type": "string", "description": "工作目录（可选）" },
                        "timeout": { "type": "integer", "description": "超时时间（秒），0 或不填表示不限时", "minimum": 0 }
                    },
                    "required": ["cmd"]
                }),
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_agent_terminal_call_is_forced_to_workspace() {
        let call = ToolCall {
            id: "call".to_string(),
            name: "run_shell".to_string(),
            arguments: serde_json::json!({ "script": "cargo check", "cwd": "/other" }),
        };

        let normalized =
            normalize_nested_terminal_call(&call, true, Some(std::path::Path::new("/workspace")))
                .unwrap();

        assert_eq!(normalized.arguments["cwd"], "/workspace");
        assert!(normalize_nested_terminal_call(&call, false, None).is_none());
    }
}
