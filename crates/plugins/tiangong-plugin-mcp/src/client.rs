//! MCP 传输客户端（stdio / HTTP via rmcp）。
//!
//! 原属 `tiangong-core::mcp::client`，MCP 管理插件化后整块迁入本 crate。

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use http::{HeaderName, HeaderValue};
use rmcp::model::{
    CallToolRequestParams, JsonObject, PaginatedRequestParams, ReadResourceRequestParams,
};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{Peer, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::config::{McpServerConfig, ResolvedMcpTransport};
use tiangong_core::process::configure_tokio_no_window;
use tiangong_core::tool::session_workspace_root;

const MAX_LIST_PAGES: usize = 8;
/// 最多保留的 stderr 末尾字节数，用于在握手失败时诊断子进程报错。
const STDERR_CAPTURE_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct McpResourceMeta {
    pub uri: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpToolMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
    #[serde(default)]
    pub argument_summaries: Vec<McpToolArgumentSummary>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpToolArgumentSummary {
    pub name: String,
    pub type_name: String,
    pub required: bool,
    pub description: String,
    pub default_value: String,
    pub enum_values: Vec<String>,
}

impl McpToolMeta {
    pub fn compact_signature(&self) -> String {
        if self.argument_summaries.is_empty() {
            return format!("{}()", self.name);
        }
        let args = self
            .argument_summaries
            .iter()
            .map(|arg| {
                let required_flag = if arg.required { "*" } else { "" };
                if arg.type_name.is_empty() {
                    format!("{}{}", arg.name, required_flag)
                } else {
                    format!("{}{}:{}", arg.name, required_flag, arg.type_name)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({})", self.name, args)
    }

    pub fn argument_brief(&self, max_items: usize) -> String {
        if self.argument_summaries.is_empty() || max_items == 0 {
            return "none".to_string();
        }
        self.argument_summaries
            .iter()
            .take(max_items)
            .map(|arg| {
                let required_flag = if arg.required { "*" } else { "" };
                let mut text = if arg.type_name.is_empty() {
                    format!("{}{}", arg.name, required_flag)
                } else {
                    format!("{}{}:{}", arg.name, required_flag, arg.type_name)
                };
                if !arg.default_value.is_empty() {
                    text.push_str(&format!("=default({})", arg.default_value));
                }
                if !arg.enum_values.is_empty() {
                    text.push_str(&format!(" enum({})", arg.enum_values.join("|")));
                }
                text
            })
            .collect::<Vec<_>>()
            .join(";")
    }
}

#[allow(dead_code)]
pub trait McpClient {
    fn list_tools(
        &self,
        server: &McpServerConfig,
        timeout_ms: u64,
    ) -> impl std::future::Future<Output = Result<Vec<McpToolMeta>>> + Send;
    fn list_resources(
        &self,
        server: &McpServerConfig,
        timeout_ms: u64,
    ) -> impl std::future::Future<Output = Result<Vec<McpResourceMeta>>> + Send;
    fn read_resource(
        &self,
        server: &McpServerConfig,
        resource: &McpResourceMeta,
        timeout_ms: u64,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
    fn call_tool(
        &self,
        server: &McpServerConfig,
        tool_name: &str,
        arguments: Value,
        timeout_ms: u64,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
}

// MCP server 版本信息全局缓存
static MCP_SERVER_VERSIONS: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn server_version_cache() -> &'static RwLock<HashMap<String, String>> {
    MCP_SERVER_VERSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn cache_server_version(name: &str, version: &str) {
    if let Ok(mut guard) = server_version_cache().write() {
        guard.insert(name.to_string(), version.to_string());
    }
}

pub fn get_cached_server_version(name: &str) -> Option<String> {
    server_version_cache()
        .read()
        .ok()
        .and_then(|guard| guard.get(name).cloned())
}

#[derive(Debug, Clone, Default)]
pub struct LocalMcpClient;

impl McpClient for LocalMcpClient {
    async fn list_tools(
        &self,
        server: &McpServerConfig,
        timeout_ms: u64,
    ) -> Result<Vec<McpToolMeta>> {
        if !server.enabled {
            return Ok(Vec::new());
        }

        if matches!(server.resolved_transport(), ResolvedMcpTransport::Metadata) {
            return Ok(server
                .tags
                .iter()
                .filter_map(|tag| {
                    let tag = tag.trim();
                    if tag.is_empty() {
                        None
                    } else {
                        Some(McpToolMeta {
                            name: tag.to_string(),
                            description: "metadata-tag".to_string(),
                            input_schema: Value::Null,
                            argument_summaries: Vec::new(),
                        })
                    }
                })
                .collect());
        }

        list_tools_via_rmcp(server, timeout_ms).await
    }

    async fn list_resources(
        &self,
        server: &McpServerConfig,
        timeout_ms: u64,
    ) -> Result<Vec<McpResourceMeta>> {
        if !server.enabled {
            return Ok(Vec::new());
        }

        if matches!(server.resolved_transport(), ResolvedMcpTransport::Metadata) {
            return Ok(server
                .tags
                .iter()
                .filter_map(|tag| {
                    let tag = tag.trim();
                    if tag.is_empty() {
                        None
                    } else {
                        Some(McpResourceMeta {
                            uri: format!("mcp://{}/{}", server.name, tag),
                        })
                    }
                })
                .collect());
        }

        list_resources_via_rmcp(server, timeout_ms).await
    }

    async fn read_resource(
        &self,
        server: &McpServerConfig,
        resource: &McpResourceMeta,
        timeout_ms: u64,
    ) -> Result<String> {
        if !server.enabled {
            return Err(anyhow!("MCP server 未启用：{}", server.name));
        }

        if matches!(server.resolved_transport(), ResolvedMcpTransport::Metadata) {
            return Ok(format!(
                "resource={} from_server={} transport=metadata command={} args={}",
                resource.uri,
                server.name,
                "(empty)",
                if server.args.is_empty() {
                    "(none)".to_string()
                } else {
                    server.args.join(" ")
                }
            ));
        }

        let result = run_mcp_request(
            server,
            timeout_ms,
            "resources/read",
            Some(json!({ "uri": resource.uri })),
        )
        .await?;
        Ok(parse_read_resource_result(&result))
    }

    async fn call_tool(
        &self,
        server: &McpServerConfig,
        tool_name: &str,
        arguments: Value,
        timeout_ms: u64,
    ) -> Result<String> {
        if !server.enabled {
            return Err(anyhow!("MCP server 未启用：{}", server.name));
        }
        if matches!(server.resolved_transport(), ResolvedMcpTransport::Metadata) {
            return Err(anyhow!(
                "MCP metadata server 不支持 tools/call：{}",
                server.name
            ));
        }

        let result = run_mcp_request(
            server,
            timeout_ms,
            "tools/call",
            Some(json!({
                "name": tool_name,
                "arguments": arguments,
            })),
        )
        .await?;
        parse_call_tool_result(&result)
    }
}

#[allow(dead_code)]
async fn list_resources_via_rmcp(
    server: &McpServerConfig,
    timeout_ms: u64,
) -> Result<Vec<McpResourceMeta>> {
    let mut resources = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor: Option<String> = None;

    for _ in 0..MAX_LIST_PAGES {
        let params = cursor.as_ref().map(|cursor| json!({ "cursor": cursor }));
        let result = run_mcp_request(server, timeout_ms, "resources/list", params).await?;
        let (page, next_cursor) = parse_resource_page(&server.name, &result);
        for resource in page {
            if seen.insert(resource.uri.clone()) {
                resources.push(resource);
            }
        }

        if let Some(next_cursor) = next_cursor {
            cursor = Some(next_cursor);
            continue;
        }
        cursor = None;
        break;
    }

    if cursor.is_some() {
        return Err(anyhow!(
            "MCP 资源分页超过上限：server={} max_pages={}",
            server.name,
            MAX_LIST_PAGES
        ));
    }

    Ok(resources)
}

async fn list_tools_via_rmcp(
    server: &McpServerConfig,
    timeout_ms: u64,
) -> Result<Vec<McpToolMeta>> {
    let mut tools = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor: Option<String> = None;

    for _ in 0..MAX_LIST_PAGES {
        let params = cursor.as_ref().map(|cursor| json!({ "cursor": cursor }));
        let result = run_mcp_request(server, timeout_ms, "tools/list", params).await?;
        let (page, next_cursor) = parse_tool_page(&server.name, &result);
        for tool in page {
            if seen.insert(tool.name.clone()) {
                tools.push(tool);
            }
        }

        if let Some(next_cursor) = next_cursor {
            cursor = Some(next_cursor);
            continue;
        }
        cursor = None;
        break;
    }

    if cursor.is_some() {
        return Err(anyhow!(
            "MCP 工具分页超过上限：server={} max_pages={}",
            server.name,
            MAX_LIST_PAGES
        ));
    }

    Ok(tools)
}

async fn run_mcp_request(
    server: &McpServerConfig,
    timeout_ms: u64,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    match server.resolved_transport() {
        ResolvedMcpTransport::Metadata => {
            return Err(anyhow!("MCP server 未配置可连接传输：{}", server.name));
        }
        ResolvedMcpTransport::Stdio => {
            if server.command_text().is_empty() {
                return Err(anyhow!("MCP server command 为空：{}", server.name));
            }
        }
        ResolvedMcpTransport::Http => {
            if server.resolved_http_endpoint().is_none() {
                return Err(anyhow!("HTTP MCP server endpoint 为空：{}", server.name));
            }
        }
    }

    let result = timeout(
        Duration::from_millis(timeout_ms),
        run_mcp_request_async(server, method, params),
    )
    .await;

    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(anyhow!(
            "MCP调用失败：server={} method={} error={}",
            server.name,
            method,
            err
        )),
        Err(_) => Err(anyhow!(
            "MCP调用超时：server={} method={} timeout_ms={}",
            server.name,
            method,
            timeout_ms
        )),
    }
}

async fn run_mcp_request_async(
    server: &McpServerConfig,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    match server.resolved_transport() {
        ResolvedMcpTransport::Http => run_http_mcp_request_async(server, method, params).await,
        ResolvedMcpTransport::Stdio => run_stdio_mcp_request_async(server, method, params).await,
        ResolvedMcpTransport::Metadata => {
            Err(anyhow!("MCP server 未配置可连接传输：{}", server.name))
        }
    }
}

async fn run_stdio_mcp_request_async(
    server: &McpServerConfig,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    let command = server.command_text();
    let args_desc = if server.args.is_empty() {
        "(none)".to_string()
    } else {
        server.args.join(" ")
    };
    let (transport, stderr_handle) =
        TokioChildProcess::builder(Command::new(command).configure(|cmd| {
            configure_tokio_no_window(cmd);
            cmd.args(&server.args);
            // stdio MCP 跟随当前会话工作目录（与 run_command 一致）；能力探测等无会话上下文
            // 的路径下此值为 None，子进程自然继承宿主进程 cwd。
            if let Some(cwd) = session_workspace_root() {
                cmd.current_dir(cwd);
            }
            for (key, value) in &server.env {
                cmd.env(key, value);
            }
        }))
        // 捕获 stderr 用于握手失败时诊断子进程报错（如 npx 下载失败、缺少 API key 等）。
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "MCP进程启动失败：server={} command={} args={}",
                server.name, command, args_desc
            )
        })?;

    // 后台持续读取 stderr，仅保留末尾窗口，避免缓冲区写满阻塞子进程。
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    if let Some(mut child_stderr) = stderr_handle {
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            loop {
                match child_stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                        let mut guard = tail.lock().await;
                        guard.push_str(&chunk);
                        if guard.len() > STDERR_CAPTURE_BYTES {
                            let start = guard.len() - STDERR_CAPTURE_BYTES;
                            let trimmed = guard.split_off(start);
                            *guard = trimmed;
                        }
                    }
                }
            }
        });
    }

    let stderr_snapshot = || -> String {
        stderr_tail
            .try_lock()
            .ok()
            .map(|guard| guard.trim().to_string())
            .unwrap_or_default()
    };

    let service = ().serve(transport).await.with_context(|| {
        let mut msg = format!("MCP握手失败：server={} command={}", server.name, command);
        if let Some(snippet) = nonempty_snippet(&stderr_snapshot()) {
            msg.push_str(" stderr=");
            msg.push_str(&snippet);
        }
        msg
    })?;

    // 缓存 server 版本信息
    if let Some(info) = service.peer_info() {
        cache_server_version(&server.name, &info.server_info.version);
    }

    let response = dispatch_mcp_request(service.peer(), method, params)
        .await
        .with_context(|| {
            let mut msg = format!(
                "MCP调用失败：server={} method={} command={}",
                server.name, method, command
            );
            if let Some(snippet) = nonempty_snippet(&stderr_snapshot()) {
                msg.push_str(" stderr=");
                msg.push_str(&snippet);
            }
            msg
        });
    let _ = service.cancel().await;
    response
}

/// 截取 stderr 末尾用于错误诊断；超长时只保留最后若干行。
fn nonempty_snippet(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    const MAX_LINES: usize = 12;
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() <= MAX_LINES {
        Some(trimmed.to_string())
    } else {
        Some(lines[lines.len() - MAX_LINES..].join("\n"))
    }
}

async fn run_http_mcp_request_async(
    server: &McpServerConfig,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    if !server.args.is_empty() {
        return Err(anyhow!(
            "HTTP MCP server 不支持 args 参数：server={} args={}",
            server.name,
            server.args.join(" ")
        ));
    }
    let endpoint = server
        .resolved_http_endpoint()
        .ok_or_else(|| anyhow!("HTTP MCP server endpoint 为空：{}", server.name))?;
    let mut transport_config = StreamableHttpClientTransportConfig::with_uri(endpoint.to_string());
    let auth_header = server.auth_header.trim();
    if !auth_header.is_empty() {
        transport_config = transport_config.auth_header(auth_header.to_string());
    }
    if !server.headers.is_empty() {
        transport_config = transport_config.custom_headers(parse_http_headers(server)?);
    }
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let service = ().serve(transport).await.with_context(|| {
        format!(
            "HTTP MCP握手失败：server={} endpoint={}",
            server.name, endpoint
        )
    })?;

    if let Some(info) = service.peer_info() {
        cache_server_version(&server.name, &info.server_info.version);
    }

    let response = dispatch_mcp_request(service.peer(), method, params).await;
    let _ = service.cancel().await;
    response
}

fn parse_http_headers(server: &McpServerConfig) -> Result<HashMap<HeaderName, HeaderValue>> {
    let mut headers = HashMap::new();
    for (key, value) in &server.headers {
        let header_name = HeaderName::from_bytes(key.trim().as_bytes())
            .with_context(|| format!("HTTP header 名称无效：server={} key={key}", server.name))?;
        let header_value = HeaderValue::from_str(value.trim()).with_context(|| {
            format!(
                "HTTP header 值无效：server={} key={} value={}",
                server.name, key, value
            )
        })?;
        headers.insert(header_name, header_value);
    }
    Ok(headers)
}

async fn dispatch_mcp_request(
    peer: &Peer<RoleClient>,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    match method {
        "tools/list" => request_tools_list(peer, params).await,
        "tools/call" => request_tools_call(peer, params).await,
        "resources/list" => request_resources_list(peer, params).await,
        "resources/read" => request_resources_read(peer, params).await,
        _ => Err(anyhow!("不支持的 MCP 方法：{method}")),
    }
}

async fn request_tools_list(peer: &Peer<RoleClient>, params: Option<Value>) -> Result<Value> {
    let cursor = params
        .as_ref()
        .and_then(|params| params.get("cursor"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|cursor| !cursor.is_empty())
        .map(ToString::to_string);
    let request = cursor.map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));

    let response = peer
        .list_tools(request)
        .await
        .context("调用 MCP tools/list 失败")?;
    serde_json::to_value(response).context("序列化 MCP tools/list 响应失败")
}

async fn request_resources_list(peer: &Peer<RoleClient>, params: Option<Value>) -> Result<Value> {
    let cursor = params
        .as_ref()
        .and_then(|params| params.get("cursor"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|cursor| !cursor.is_empty())
        .map(ToString::to_string);
    let request = cursor.map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));

    let response = peer
        .list_resources(request)
        .await
        .context("调用 MCP resources/list 失败")?;
    serde_json::to_value(response).context("序列化 MCP resources/list 响应失败")
}

async fn request_resources_read(peer: &Peer<RoleClient>, params: Option<Value>) -> Result<Value> {
    let uri = params
        .as_ref()
        .and_then(|params| params.get("uri"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|uri| !uri.is_empty())
        .ok_or_else(|| anyhow!("MCP resources/read 缺少 uri 参数"))?
        .to_string();

    let response = peer
        .read_resource(ReadResourceRequestParams::new(uri))
        .await
        .context("调用 MCP resources/read 失败")?;
    serde_json::to_value(response).context("序列化 MCP resources/read 响应失败")
}

async fn request_tools_call(peer: &Peer<RoleClient>, params: Option<Value>) -> Result<Value> {
    let params = params.unwrap_or_else(|| json!({}));
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("MCP tools/call 缺少 name 参数"))?
        .to_string();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let arguments = if arguments.is_null() {
        None
    } else {
        match arguments {
            Value::Object(_) => Some(
                serde_json::from_value::<JsonObject>(arguments)
                    .context("MCP tools/call arguments 必须是对象")?,
            ),
            _ => return Err(anyhow!("MCP tools/call arguments 必须是对象")),
        }
    };
    let response = peer
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments.unwrap_or_default()))
        .await
        .context("调用 MCP tools/call 失败")?;
    serde_json::to_value(response).context("序列化 MCP tools/call 响应失败")
}

#[allow(dead_code)]
fn parse_resource_page(
    _server_name: &str,
    value: &Value,
) -> (Vec<McpResourceMeta>, Option<String>) {
    match value {
        Value::Array(items) => (
            items
                .iter()
                .filter_map(parse_resource_uri)
                .map(|uri| McpResourceMeta { uri })
                .collect(),
            None,
        ),
        Value::Object(map) => {
            let mut resources = map
                .get("resources")
                .map(|resources| parse_resource_page(_server_name, resources).0)
                .unwrap_or_default();

            if resources.is_empty()
                && let Some(uri) = parse_resource_uri(value)
            {
                resources.push(McpResourceMeta { uri });
            }

            let next_cursor = map
                .get("nextCursor")
                .and_then(Value::as_str)
                .or_else(|| map.get("next_cursor").and_then(Value::as_str))
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToString::to_string);

            (resources, next_cursor)
        }
        _ => (Vec::new(), None),
    }
}

fn parse_tool_page(server_name: &str, value: &Value) -> (Vec<McpToolMeta>, Option<String>) {
    match value {
        Value::Array(items) => (
            items
                .iter()
                .filter_map(|item| parse_tool_item(server_name, item))
                .collect(),
            None,
        ),
        Value::Object(map) => {
            let mut tools = map
                .get("tools")
                .map(|tools| parse_tool_page(server_name, tools).0)
                .unwrap_or_default();

            if tools.is_empty()
                && let Some(tool) = parse_tool_item(server_name, value)
            {
                tools.push(tool);
            }

            let next_cursor = map
                .get("nextCursor")
                .and_then(Value::as_str)
                .or_else(|| map.get("next_cursor").and_then(Value::as_str))
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToString::to_string);

            (tools, next_cursor)
        }
        _ => (Vec::new(), None),
    }
}

fn parse_tool_item(_server_name: &str, value: &Value) -> Option<McpToolMeta> {
    match value {
        Value::String(name) => {
            let name = name.trim();
            if name.is_empty() {
                None
            } else {
                Some(McpToolMeta {
                    name: name.to_string(),
                    description: String::new(),
                    input_schema: Value::Null,
                    argument_summaries: Vec::new(),
                })
            }
        }
        Value::Object(map) => {
            let name = map
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())?;
            let description = map
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            let input_schema = map
                .get("inputSchema")
                .or_else(|| map.get("input_schema"))
                .cloned()
                .unwrap_or(Value::Null);
            Some(McpToolMeta {
                name: name.to_string(),
                description,
                argument_summaries: parse_tool_argument_summaries(&input_schema),
                input_schema,
            })
        }
        _ => None,
    }
}

fn parse_tool_argument_summaries(input_schema: &Value) -> Vec<McpToolArgumentSummary> {
    let Value::Object(schema) = input_schema else {
        return Vec::new();
    };
    let required_set = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToString::to_string)
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut output = Vec::new();
    for (name, prop) in properties {
        let prop_obj = prop.as_object();
        let type_name = prop_obj
            .and_then(|obj| obj.get("type"))
            .map(parse_schema_type_name)
            .unwrap_or_default();
        let description = prop_obj
            .and_then(|obj| obj.get("description"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        let default_value = prop_obj
            .and_then(|obj| obj.get("default"))
            .map(compact_json)
            .unwrap_or_default();
        let enum_values = prop_obj
            .and_then(|obj| obj.get("enum"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(compact_json)
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        output.push(McpToolArgumentSummary {
            name: name.to_string(),
            type_name,
            required: required_set.contains(name),
            description,
            default_value,
            enum_values,
        });
    }
    output.sort_by(|a, b| a.name.cmp(&b.name));
    output
}

fn parse_schema_type_name(value: &Value) -> String {
    match value {
        Value::String(name) => name.trim().to_string(),
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>()
            .join("|"),
        _ => String::new(),
    }
}

fn compact_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => v.to_string(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

#[allow(dead_code)]
fn parse_resource_uri(value: &Value) -> Option<String> {
    match value {
        Value::String(uri) => {
            let uri = uri.trim();
            if uri.is_empty() {
                None
            } else {
                Some(uri.to_string())
            }
        }
        Value::Object(map) => {
            let uri = map
                .get("uri")
                .and_then(Value::as_str)
                .or_else(|| map.get("name").and_then(Value::as_str))
                .unwrap_or("")
                .trim();
            if uri.is_empty() {
                None
            } else {
                Some(uri.to_string())
            }
        }
        _ => None,
    }
}

#[allow(dead_code)]
fn parse_read_resource_result(result: &Value) -> String {
    if let Some(content) = parse_content_from_json(result) {
        return content;
    }

    result.to_string()
}

fn parse_call_tool_result(result: &Value) -> Result<String> {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .or_else(|| result.get("is_error").and_then(Value::as_bool))
        .unwrap_or(false);

    if let Some(value) = result
        .get("structuredContent")
        .or_else(|| result.get("structured_content"))
    {
        if is_error {
            return Err(anyhow!("MCP tools/call 返回错误：{}", compact_json(value)));
        }
        return Ok(compact_json(value));
    }

    let content = result.get("content").cloned().unwrap_or(Value::Null);
    let parsed = parse_content_from_json(&content).unwrap_or_else(|| compact_json(&content));
    if is_error {
        Err(anyhow!("MCP tools/call 返回错误：{}", parsed))
    } else {
        Ok(parsed)
    }
}

#[allow(dead_code)]
fn parse_content_from_json(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(parse_content_from_json)
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(map) => {
            for key in ["content", "text", "data", "blob"] {
                if let Some(raw) = map.get(key)
                    && let Some(parsed) = parse_content_from_json(raw)
                {
                    return Some(parsed);
                }
            }

            if let Some(contents) = map.get("contents") {
                return parse_content_from_json(contents);
            }
            None
        }
        _ => None,
    }
}
