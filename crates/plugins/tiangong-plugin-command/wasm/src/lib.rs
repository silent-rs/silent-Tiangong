//! Command 插件的 WASM 桥接组件。
//!
//! 本组件只做桥接：工具规格、参数解析、prompt 段落与生命周期入口；run_command /
//! run_shell 经 sidecar.invoke 转发（tokio 子进程 spawn、命令校验、env 注入全部在
//! sidecar 进程内）。wasm 侧不做任何校验（路径越界依赖文件系统，整体下沉 sidecar，
//! 与 fetch/fs 改造一致）。

mod bindings;
mod sidecar_client;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use tiangong_plugin_command_protocol::exec::{
    ExecResponse, RunCommand, RunCommandRequest, RunShell, RunShellRequest, SetWorkspace,
    SetWorkspaceRequest,
};
use tiangong_plugin_command_protocol::{TOOL_RUN_COMMAND, TOOL_RUN_SHELL};

mod descriptor {
    pub const ID: &str = tiangong_plugin_command_protocol::PLUGIN_ID;
    pub const NAME: &str = "Command";
    pub const VERSION: &str = tiangong_plugin_command_protocol::PLUGIN_VERSION;
}

/// 全局状态缓存（WASM 单线程，RefCell 安全）。
mod state {
    use std::cell::RefCell;

    struct PluginState {
        workspace: Option<String>,
        full_trust: bool,
        allowed_commands: Vec<String>,
    }

    thread_local! {
        static STATE: RefCell<PluginState> = const { RefCell::new(PluginState {
            workspace: None,
            full_trust: false,
            allowed_commands: Vec::new(),
        }) };
    }

    pub fn set_workspace(ws: Option<String>) {
        STATE.with(|s| s.borrow_mut().workspace = ws);
    }

    pub fn set_full_trust(full_trust: bool) {
        STATE.with(|s| s.borrow_mut().full_trust = full_trust);
    }

    pub fn set_allowed_commands(cmds: Vec<String>) {
        STATE.with(|s| s.borrow_mut().allowed_commands = cmds);
    }

    /// 构造访问上下文（沙箱预留点 B：未来扩展此结构即可细化权限）。
    pub fn access_context() -> tiangong_plugin_command_protocol::CommandAccessContext {
        STATE.with(|s| {
            let s = s.borrow();
            tiangong_plugin_command_protocol::CommandAccessContext {
                workspace: s.workspace.clone(),
                full_trust: s.full_trust,
                allowed_commands: s.allowed_commands.clone(),
            }
        })
    }

    /// 取当前缓存的 workspace/full_trust/allowed_commands 用于变更检测。
    pub fn snapshot() -> (Option<String>, bool, Vec<String>) {
        STATE.with(|s| {
            let s = s.borrow();
            (
                s.workspace.clone(),
                s.full_trust,
                s.allowed_commands.clone(),
            )
        })
    }
}

fn plugin_err(message: impl Into<String>) -> PluginError {
    PluginError::Message(message.into())
}

struct Component;

impl Guest for Component {
    fn describe() -> Result<PluginDescriptor, PluginError> {
        Ok(PluginDescriptor {
            id: descriptor::ID.to_string(),
            name: descriptor::NAME.to_string(),
            version: descriptor::VERSION.to_string(),
        })
    }

    fn tool_specs() -> Result<Vec<ToolSpec>, PluginError> {
        Ok(vec![
            ToolSpec {
                name: TOOL_RUN_COMMAND.to_string(),
                description: "执行受控命令，支持 cwd 和超时设置。shell 脚本建议使用 run_shell"
                    .to_string(),
                input_schema: serde_json::to_string(&serde_json::json!({
                    "type": "object",
                    "properties": {
                        "cmd": { "type": "string", "description": "命令名（可含参数，自动拆分）" },
                        "args": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "命令参数列表"
                        },
                        "cwd": { "type": "string", "description": "工作目录（可选）" },
                        "timeout": { "type": "integer", "description": "超时时间（秒），0 或不填表示不限时", "minimum": 0 }
                    },
                    "required": ["cmd"]
                }))
                .unwrap_or_else(|_| "{}".to_string()),
            },
            ToolSpec {
                name: TOOL_RUN_SHELL.to_string(),
                description: "执行 shell 脚本，自动派生 bash/sh/powershell 参数。".to_string(),
                input_schema: serde_json::to_string(&serde_json::json!({
                    "type": "object",
                    "properties": {
                        "script": { "type": "string", "description": "shell 脚本文本" },
                        "shell": { "type": "string", "description": "shell 类型：auto/bash/sh/powershell/pwsh，默认 auto" },
                        "cwd": { "type": "string", "description": "工作目录（可选）" },
                        "timeout": { "type": "integer", "description": "超时时间（秒），0 或不填表示不限时", "minimum": 0 }
                    },
                    "required": ["script"]
                }))
                .unwrap_or_else(|_| "{}".to_string()),
            },
        ])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        // 原 core rules 第 8 条（run_command 部分）：命令执行默认走 run_command。
        Ok(vec![
            "命令执行默认使用 run_command，并根据工具结果继续推进。".to_string(),
        ])
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_RUN_COMMAND => handle_run_command(call.arguments),
            TOOL_RUN_SHELL => handle_run_shell(call.arguments),
            // run_bash 作为 run_shell 的 shell=bash 特例（不暴露 ToolSpec，与现状一致）。
            "run_bash" => handle_run_shell(call.arguments),
            other => Err(plugin_err(format!("未知的 Command 工具: {other}"))),
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(workspace: Option<String>, full_trust: bool) -> Result<(), PluginError> {
        // 工作目录和信任模式都未变时，跳过 sidecar 调用。
        let (ws, ft, _) = state::snapshot();
        if ws == workspace && ft == full_trust {
            return Ok(());
        }
        state::set_workspace(workspace.clone());
        state::set_full_trust(full_trust);
        // 通知 sidecar 工作区与信任模式变更。
        let (_, _, allowed_commands) = state::snapshot();
        let request = SetWorkspaceRequest {
            workspace,
            full_trust,
            allowed_commands,
        };
        sidecar_client::invoke::<SetWorkspace>(&request)
            .map_err(|error| plugin_err(format!("set_workspace 调用 sidecar 失败: {error}")))?;
        Ok(())
    }

    fn on_config_updated(config_json: String) -> Result<(), PluginError> {
        // 解析 CoreConfig 里的 allowed_commands（用户扩展白名单）并缓存 + 推送 sidecar。
        // 与原进程内插件语义对齐：allowed_commands 经 on_config_updated 注入。
        let config: serde_json::Value = serde_json::from_str(&config_json).unwrap_or_default();
        let allowed: Vec<String> = config
            .get("allowed_commands")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if allowed.is_empty() {
            return Ok(());
        }
        state::set_allowed_commands(allowed.clone());
        // 推送给 sidecar（携带当前 workspace/full_trust）。
        let (ws, ft, _) = state::snapshot();
        let request = SetWorkspaceRequest {
            workspace: ws,
            full_trust: ft,
            allowed_commands: allowed,
        };
        sidecar_client::invoke::<SetWorkspace>(&request)
            .map_err(|error| plugin_err(format!("on_config_updated 推送失败: {error}")))?;
        Ok(())
    }

    fn on_session_ready(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_started(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_finished(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ended(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }
}

/// run_command：解析参数 → 组装请求（带 CommandAccessContext）→ invoke sidecar。
fn handle_run_command(arguments: String) -> Result<ToolResult, PluginError> {
    let args: serde_json::Value = serde_json::from_str(&arguments).unwrap_or(serde_json::json!({}));
    let request = RunCommandRequest {
        cmd: args
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        args: args
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        cwd: args
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .map(String::from),
        timeout_secs: args
            .get("timeout")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0),
        access: state::access_context(),
    };
    if request.cmd.trim().is_empty() {
        return Ok(tool_failure("run_command 缺少 cmd 参数", "missing cmd"));
    }
    let resp: ExecResponse = sidecar_client::invoke::<RunCommand>(&request)
        .map_err(|e| plugin_err(format!("run_command 执行失败: {e}")))?;
    Ok(to_tool_result(resp))
}

/// run_shell：解析参数 → 组装请求 → invoke sidecar。
fn handle_run_shell(arguments: String) -> Result<ToolResult, PluginError> {
    let args: serde_json::Value = serde_json::from_str(&arguments).unwrap_or(serde_json::json!({}));
    let request = RunShellRequest {
        script: args
            .get("script")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        shell: args
            .get("shell")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("auto")
            .to_string(),
        cwd: args
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .map(String::from),
        timeout_secs: args
            .get("timeout")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0),
        access: state::access_context(),
    };
    if request.script.trim().is_empty() {
        return Ok(tool_failure("run_shell 缺少 script 参数", "missing script"));
    }
    let resp: ExecResponse = sidecar_client::invoke::<RunShell>(&request)
        .map_err(|e| plugin_err(format!("run_shell 执行失败: {e}")))?;
    Ok(to_tool_result(resp))
}

/// ExecResponse → WIT ToolResult。
fn to_tool_result(resp: ExecResponse) -> ToolResult {
    ToolResult {
        ok: resp.ok,
        summary: resp.summary,
        stdout: resp.stdout,
        stderr: resp.stderr,
        exit_code: resp.exit_code,
        execution: None,
    }
}

/// 构造简单失败 ToolResult。
fn tool_failure(summary: &str, stderr: &str) -> ToolResult {
    ToolResult {
        ok: false,
        summary: summary.to_string(),
        stdout: String::new(),
        stderr: stderr.to_string(),
        exit_code: 1,
        execution: None,
    }
}

/// Command 插件无设置页：contributions 返回空，其余入口报错。
impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("Command 插件暂无设置页面"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("Command 插件暂无页面资源"))
    }

    fn handle_view_message(
        _request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        Err(plugin_err("Command 插件暂无页面消息"))
    }
}

bindings::export!(Component with_types_in bindings);
