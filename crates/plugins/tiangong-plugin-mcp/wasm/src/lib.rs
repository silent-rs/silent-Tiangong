//! MCP 插件的 WASM 桥接组件。
//!
//! 本组件只做桥接：工具规格/工具执行/管理操作全部转发到 MCP sidecar。
//! MCP 工具是动态的（运行时探测），`tool_specs` 每次从 sidecar 拉取；
//! `handle_tool` 解析 `mcp__{server}__{tool}` 名后转发执行。

mod bindings;
mod sidecar_client;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolExecutionRecord, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use tiangong_plugin_mcp_protocol::capability::Reconfigure;
use tiangong_plugin_mcp_protocol::config::{RegisterMcpServerRequest, UpdateConfigEntryRequest};
use tiangong_plugin_mcp_protocol::management::{
    ConfigGet, ConfigSnapshot, RemoveServerRequest, ServerMergeDisk, ServerRegister, ServerRemove,
    ServerSetEnabled, ServerUpdate, SetEnabledRequest, UpdateServerRequest,
};
use tiangong_plugin_mcp_protocol::query::{
    ServerCachedTools, ServerDetail, ServerHealth, ServerList, ServerNameRequest, ServerSummary,
};
use tiangong_plugin_mcp_protocol::tool::{
    ExecuteTool, ExecuteToolRequest, ExecuteToolResponse, ListTools, ListToolsResponse,
};
use tiangong_plugin_mcp_protocol::{Empty, McpOperation, MessageResponse, NameFilterRequest};

mod descriptor {
    pub const ID: &str = tiangong_plugin_mcp_protocol::PLUGIN_ID;
    pub const NAME: &str = "MCP";
    pub const VERSION: &str = tiangong_plugin_mcp_protocol::PLUGIN_VERSION;
}

/// 全局状态缓存（WASM 单线程，RefCell 安全）。
mod state {
    use std::cell::RefCell;

    struct PluginState {
        workspace: Option<String>,
    }

    thread_local! {
        static STATE: RefCell<PluginState> = const { RefCell::new(PluginState { workspace: None }) };
    }

    pub fn set_workspace(ws: Option<String>) {
        STATE.with(|s| s.borrow_mut().workspace = ws);
    }

    pub fn workspace() -> Option<String> {
        STATE.with(|s| s.borrow().workspace.clone())
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
        // 动态工具：每次从 sidecar 拉取健康 server 的工具列表。
        let response: ListToolsResponse = match sidecar_client::invoke::<ListTools>(&Empty {}) {
            Ok(response) => response,
            Err(error) => {
                tracing_like_warn(&format!("MCP tool_specs 拉取失败: {error}"));
                return Ok(Vec::new());
            }
        };
        let specs = response
            .servers
            .into_iter()
            .flat_map(|entry| {
                entry
                    .tools
                    .into_iter()
                    .map(move |tool| (entry.server.clone(), tool))
            })
            .map(|(server, tool)| {
                let function_name = resolve_mcp_function_name(&server, &tool.name);
                ToolSpec {
                    name: function_name,
                    description: format!(
                        "MCP调用：server={} tool={} description={}",
                        server, tool.name, tool.description
                    ),
                    input_schema: if tool.input_schema.is_object() {
                        serde_json::to_string(&tool.input_schema)
                            .unwrap_or_else(|_| "{}".to_string())
                    } else {
                        "{}".to_string()
                    },
                }
            })
            .collect();
        Ok(specs)
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        // MCP 工具仅经 function-calling 暴露，不注入 prompt 段落。
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        // 解析 mcp__{server}__{tool} → (server, tool)
        let (server_name, tool_name) = parse_mcp_function_name(&call.name)
            .ok_or_else(|| plugin_err(format!("不是 MCP 工具：{}", call.name)))?;
        let arguments: serde_json::Value =
            serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
        let request = ExecuteToolRequest {
            server_name,
            tool_name,
            arguments,
            workspace: state::workspace(),
        };
        let response: ExecuteToolResponse = sidecar_client::invoke::<ExecuteTool>(&request)
            .map_err(|e| plugin_err(format!("MCP 工具执行失败: {e}")))?;
        Ok(ToolResult {
            ok: response.ok,
            summary: response.summary,
            stdout: response.stdout,
            stderr: response.stderr,
            exit_code: response.exit_code as i32,
            execution: Some(ToolExecutionRecord {
                tool_name: response.tool_name,
                args: response.arguments,
                duration_ms: response.duration_ms,
                ok: response.ok,
                exit_code: response.exit_code as i32,
                summary: String::new(),
            }),
        })
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(workspace: Option<String>) -> Result<(), PluginError> {
        state::set_workspace(workspace);
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        // 配置变更时通知 sidecar 重新探测 capability。
        let request = ReconfigureRequest {
            workspace: state::workspace(),
        };
        let _ = sidecar_client::invoke::<Reconfigure>(&request);
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

use tiangong_plugin_mcp_protocol::capability::ReconfigureRequest;

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(vec![Contribution {
            id: "mcp-settings".to_string(),
            title: "MCP 管理".to_string(),
            description: "管理 MCP server 配置".to_string(),
            icon: "plug".to_string(),
            group: "tools".to_string(),
            has_view: true,
        }])
    }

    fn open_view(_contribution_id: String) -> Result<ViewResponse, PluginError> {
        Ok(ViewResponse {
            html: mcp_settings_html(),
        })
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        // 单文件内联页面，无外部资源。
        Ok(ResourceResponse {
            data: Vec::new(),
            mime: "text/plain".to_string(),
        })
    }

    fn handle_view_message(
        request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        let payload = match request.method.as_str() {
            "bootstrap" => invoke_for_ui::<ConfigSnapshot>(&Empty {})?,
            "config.get" => invoke_for_ui::<ConfigGet>(&Empty {})?,
            "config.update_entry" => {
                use tiangong_plugin_mcp_protocol::management::UpdateConfigEntry;
                let req: UpdateConfigEntryRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析配置更新请求失败: {e}")))?;
                let resp: MessageResponse = sidecar_client::invoke::<UpdateConfigEntry>(&req)
                    .map_err(|e| plugin_err(e.to_string()))?;
                serde_json::to_string(&resp).map_err(|e| plugin_err(e.to_string()))?
            }
            "server.register" => {
                let req: RegisterMcpServerRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析注册请求失败: {e}")))?;
                let resp: MessageResponse = sidecar_client::invoke::<ServerRegister>(&req)
                    .map_err(|e| plugin_err(e.to_string()))?;
                serde_json::to_string(&resp).map_err(|e| plugin_err(e.to_string()))?
            }
            "server.update" => {
                let req: UpdateServerRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析更新请求失败: {e}")))?;
                let resp: MessageResponse = sidecar_client::invoke::<ServerUpdate>(&req)
                    .map_err(|e| plugin_err(e.to_string()))?;
                serde_json::to_string(&resp).map_err(|e| plugin_err(e.to_string()))?
            }
            "server.remove" => {
                let req: RemoveServerRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析删除请求失败: {e}")))?;
                let resp: MessageResponse = sidecar_client::invoke::<ServerRemove>(&req)
                    .map_err(|e| plugin_err(e.to_string()))?;
                serde_json::to_string(&resp).map_err(|e| plugin_err(e.to_string()))?
            }
            "server.set_enabled" => {
                let req: SetEnabledRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析启停请求失败: {e}")))?;
                let resp: MessageResponse = sidecar_client::invoke::<ServerSetEnabled>(&req)
                    .map_err(|e| plugin_err(e.to_string()))?;
                serde_json::to_string(&resp).map_err(|e| plugin_err(e.to_string()))?
            }
            "server.list" => invoke_for_ui::<ServerList>(&Empty {})?,
            "server.cached_tools" => {
                let req: ServerNameRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析请求失败: {e}")))?;
                invoke_for_ui::<ServerCachedTools>(&req)?
            }
            "server.health" => invoke_for_ui::<ServerHealth>(&Empty {})?,
            "server.summary" => {
                let req: NameFilterRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析请求失败: {e}")))?;
                invoke_for_ui::<ServerSummary>(&req)?
            }
            "server.detail" => {
                let req: NameFilterRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析请求失败: {e}")))?;
                invoke_for_ui::<ServerDetail>(&req)?
            }
            "server.probe" => {
                use tiangong_plugin_mcp_protocol::capability::ServerProbe;
                let req: ServerNameRequest = serde_json::from_str(&request.payload)
                    .map_err(|e| plugin_err(format!("解析请求失败: {e}")))?;
                let resp: Empty = sidecar_client::invoke::<ServerProbe>(&req)
                    .map_err(|e| plugin_err(e.to_string()))?;
                serde_json::to_string(&resp).map_err(|e| plugin_err(e.to_string()))?
            }
            "server.merge_disk" => {
                let resp: MessageResponse = sidecar_client::invoke::<ServerMergeDisk>(&Empty {})
                    .map_err(|e| plugin_err(e.to_string()))?;
                serde_json::to_string(&resp).map_err(|e| plugin_err(e.to_string()))?
            }
            other => return Err(plugin_err(format!("未知的 MCP 页面消息: {other}"))),
        };
        Ok(ViewMessageResponse { payload })
    }
}

fn invoke_for_ui<O>(request: &O::Request) -> Result<String, PluginError>
where
    O: McpOperation,
    O::Response: serde::Serialize,
{
    let response = sidecar_client::invoke::<O>(request).map_err(|e| plugin_err(e.to_string()))?;
    serde_json::to_string(&response).map_err(|e| plugin_err(e.to_string()))
}

/// 生成 MCP 管理页 HTML（单文件内联）。
fn mcp_settings_html() -> String {
    "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>MCP 管理</title></head>\
     <body><div id=\"app\">MCP 管理页面（请通过消息接口操作）</div></body></html>"
        .to_string()
}

/// WASM 内简易日志（经 feedback 通道发事件，当前实现暂作 no-op）。
fn tracing_like_warn(message: &str) {
    let _ = message;
}

/// 生成 MCP 工具的 LLM 可见函数名：`mcp__{server}__{tool}`（与 sidecar 命名一致）。
fn resolve_mcp_function_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_fn_name(server_name),
        sanitize_fn_name(tool_name)
    )
}

/// 解析 `mcp__{server}__{tool}` → (server, tool)；非 MCP 工具返回 None。
fn parse_mcp_function_name(function_name: &str) -> Option<(String, String)> {
    let rest = function_name.strip_prefix("mcp__")?;
    let separator = rest.find("__")?;
    let server = &rest[..separator];
    let tool = &rest[separator + 2..];
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

fn sanitize_fn_name(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "mcp_tool".to_string()
    } else {
        trimmed
    }
}

bindings::export!(Component with_types_in bindings);
