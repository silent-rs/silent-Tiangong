//! Fetch 插件的 WASM 桥接组件。
//!
//! 本组件只做桥接：工具规格与工具执行全部转发到 Fetch sidecar。重型原生依赖
//!（reqwest 阻塞抓取、SSRF 防护、download 落盘）全部在 sidecar 进程内运行，
//! WASM 沙箱仅负责参数解析与 IPC 转发。

mod bindings;
mod sidecar_client;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use tiangong_plugin_fetch_protocol::web_fetch::{
    SetWorkspace, SetWorkspaceRequest, WebFetch, WebFetchRequest,
};
use tiangong_plugin_fetch_protocol::{ExtractMode, FetchMode, TOOL_WEB_FETCH};

mod descriptor {
    pub const ID: &str = tiangong_plugin_fetch_protocol::PLUGIN_ID;
    pub const NAME: &str = "Fetch";
    pub const VERSION: &str = tiangong_plugin_fetch_protocol::PLUGIN_VERSION;
}

/// 全局状态缓存（WASM 单线程，RefCell 安全）。
mod state {
    use std::cell::RefCell;

    struct PluginState {
        workspace: Option<String>,
        full_trust: bool,
    }

    thread_local! {
        static STATE: RefCell<PluginState> = const { RefCell::new(PluginState {
            workspace: None,
            full_trust: false,
        }) };
    }

    pub fn set_workspace(ws: Option<String>) {
        STATE.with(|s| s.borrow_mut().workspace = ws);
    }

    pub fn set_full_trust(full_trust: bool) {
        STATE.with(|s| s.borrow_mut().full_trust = full_trust);
    }

    pub fn full_trust() -> bool {
        STATE.with(|s| s.borrow().full_trust)
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
        Ok(vec![ToolSpec {
            name: TOOL_WEB_FETCH.to_string(),
            description: "获取 URL 内容。支持 HTTP/HTTPS 网页（text 模式提取正文 / download 模式落盘），含 SSRF 防护。"
                .to_string(),
            input_schema: serde_json::to_string(&serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "要获取的 HTTP/HTTPS URL" },
                    "mode": { "type": "string", "enum": ["text", "download"], "description": "执行模式，默认 text" },
                    "max_chars": { "type": "integer", "description": "text 模式最多返回字符数，默认 12000，最大 50000", "minimum": 1, "maximum": 50000 },
                    "output_path": { "type": "string", "description": "download 模式目标文件路径，必须位于允许写入目录" },
                    "overwrite": { "type": "boolean", "description": "download 模式是否覆盖已有文件，默认 false" },
                    "timeout_ms": { "type": "integer", "description": "请求超时时间，默认 15000，最大 60000", "minimum": 1000, "maximum": 60000 },
                    "follow_redirects": { "type": "boolean", "description": "是否跟随重定向，默认 true" },
                    "extract_mode": { "type": "string", "enum": ["auto", "text", "raw"], "description": "text 模式提取方式，默认 auto" }
                },
                "required": ["url"]
            }))
            .unwrap_or_else(|_| "{}".to_string()),
        }])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(Vec::new())
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_WEB_FETCH => handle_web_fetch(&call),
            other => Err(plugin_err(format!("未知的 Fetch 工具: {other}"))),
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(workspace: Option<String>, full_trust: bool) -> Result<(), PluginError> {
        // 工作目录和信任模式都未变时，跳过 sidecar 调用，避免每轮消息都做一次同步 IPC。
        let unchanged = state::workspace() == workspace && state::full_trust() == full_trust;
        if unchanged {
            return Ok(());
        }
        state::set_workspace(workspace.clone());
        state::set_full_trust(full_trust);
        // 通知 sidecar 工作区变更（download 落盘基准）。
        let request = SetWorkspaceRequest { workspace };
        sidecar_client::invoke::<SetWorkspace>(&request)
            .map_err(|error| plugin_err(format!("set_workspace 调用 sidecar 失败: {error}")))?;
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
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

/// 处理 web_fetch 工具：从 ToolCall.arguments 解析命名参数，组装 WebFetchRequest
/// 转发到 sidecar，把响应组装成 ToolResult。
fn handle_web_fetch(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: serde_json::Value =
        serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));

    let url = args
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if url.is_empty() {
        return Ok(tool_failure("web_fetch 缺少 url 参数", "missing url"));
    }

    let mode = match args.get("mode").and_then(serde_json::Value::as_str) {
        Some("download") => FetchMode::Download,
        _ => FetchMode::Text,
    };
    let extract_mode = match args.get("extract_mode").and_then(serde_json::Value::as_str) {
        Some("text") => ExtractMode::Text,
        Some("raw") => ExtractMode::Raw,
        _ => ExtractMode::Auto,
    };
    let request = WebFetchRequest {
        url,
        mode,
        max_chars: args
            .get("max_chars")
            .and_then(|v| {
                v.as_u64()
                    .map(|n| n as usize)
                    .or_else(|| v.as_str().and_then(|s| s.parse::<usize>().ok()))
            })
            .unwrap_or(12_000),
        output_path: args
            .get("output_path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string),
        overwrite: args
            .get("overwrite")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        timeout_ms: args
            .get("timeout_ms")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .unwrap_or(15_000),
        follow_redirects: args
            .get("follow_redirects")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        extract_mode,
    };

    // 注意：URL 解析与 SSRF 校验在 sidecar 内执行（DNS 解析、IP 判定需要原生网络栈）。
    let response = sidecar_client::invoke::<WebFetch>(&request)
        .map_err(|e| plugin_err(format!("web_fetch 执行失败: {e}")))?;
    Ok(ToolResult {
        ok: response.ok,
        summary: response.summary,
        stdout: response.stdout,
        stderr: response.stderr,
        exit_code: response.exit_code,
        execution: None,
    })
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

/// Fetch 插件无设置页：contributions 返回空，其余入口报错。
impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("Fetch 插件暂无设置页面"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("Fetch 插件暂无页面资源"))
    }

    fn handle_view_message(
        _request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        Err(plugin_err("Fetch 插件暂无页面消息"))
    }
}
bindings::export!(Component with_types_in bindings);
