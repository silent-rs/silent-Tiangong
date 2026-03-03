use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use http::{HeaderName, HeaderValue};
use rmcp::model::{PaginatedRequestParams, ReadResourceRequestParams};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{Peer, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use crate::core::agent_config::{McpServerConfig, ResolvedMcpTransport};

const MAX_LIST_PAGES: usize = 8;

#[derive(Debug, Clone)]
pub struct McpResourceMeta {
    pub server: String,
    pub uri: String,
}

pub trait McpClient {
    fn list_resources(
        &self,
        server: &McpServerConfig,
        timeout_ms: u64,
    ) -> Result<Vec<McpResourceMeta>>;
    fn read_resource(
        &self,
        server: &McpServerConfig,
        resource: &McpResourceMeta,
        timeout_ms: u64,
    ) -> Result<String>;
}

#[derive(Debug, Clone, Default)]
pub struct LocalMcpClient;

impl McpClient for LocalMcpClient {
    fn list_resources(
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
                            server: server.name.clone(),
                            uri: format!("mcp://{}/{}", server.name, tag),
                        })
                    }
                })
                .collect());
        }

        list_resources_via_rmcp(server, timeout_ms)
    }

    fn read_resource(
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
        )?;
        Ok(parse_read_resource_result(&result))
    }
}

fn list_resources_via_rmcp(
    server: &McpServerConfig,
    timeout_ms: u64,
) -> Result<Vec<McpResourceMeta>> {
    let mut resources = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor: Option<String> = None;

    for _ in 0..MAX_LIST_PAGES {
        let params = cursor.as_ref().map(|cursor| json!({ "cursor": cursor }));
        let result = run_mcp_request(server, timeout_ms, "resources/list", params)?;
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

fn run_mcp_request(
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

    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化 MCP 运行时失败")?;

    let output = runtime.block_on(async {
        timeout(
            Duration::from_millis(timeout_ms),
            run_mcp_request_async(server, method, params),
        )
        .await
    });

    match output {
        Ok(Ok(result)) => Ok(result),
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
    let transport = TokioChildProcess::new(Command::new(command).configure(|cmd| {
        cmd.args(&server.args);
        cmd.stderr(Stdio::null());
        if let Some(cwd) = server.cwd_text() {
            cmd.current_dir(cwd);
        }
        for (key, value) in &server.env {
            cmd.env(key, value);
        }
    }))
    .with_context(|| {
        format!(
            "MCP进程启动失败：server={} command={} args={}",
            server.name,
            command,
            if server.args.is_empty() {
                "(none)".to_string()
            } else {
                server.args.join(" ")
            }
        )
    })?;

    let service = ()
        .serve(transport)
        .await
        .with_context(|| format!("MCP握手失败：server={} command={}", server.name, command))?;

    let response = dispatch_mcp_request(service.peer(), method, params).await;
    let _ = service.cancel().await;
    response
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
        "resources/list" => request_resources_list(peer, params).await,
        "resources/read" => request_resources_read(peer, params).await,
        _ => Err(anyhow!("不支持的 MCP 方法：{method}")),
    }
}

async fn request_resources_list(peer: &Peer<RoleClient>, params: Option<Value>) -> Result<Value> {
    let cursor = params
        .as_ref()
        .and_then(|params| params.get("cursor"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|cursor| !cursor.is_empty())
        .map(ToString::to_string);
    let request = cursor.map(|cursor| PaginatedRequestParams {
        meta: None,
        cursor: Some(cursor),
    });

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
        .read_resource(ReadResourceRequestParams { meta: None, uri })
        .await
        .context("调用 MCP resources/read 失败")?;
    serde_json::to_value(response).context("序列化 MCP resources/read 响应失败")
}

fn parse_resource_page(server_name: &str, value: &Value) -> (Vec<McpResourceMeta>, Option<String>) {
    match value {
        Value::Array(items) => (
            items
                .iter()
                .filter_map(parse_resource_uri)
                .map(|uri| McpResourceMeta {
                    server: server_name.to_string(),
                    uri,
                })
                .collect(),
            None,
        ),
        Value::Object(map) => {
            let mut resources = map
                .get("resources")
                .map(|resources| parse_resource_page(server_name, resources).0)
                .unwrap_or_default();

            if resources.is_empty()
                && let Some(uri) = parse_resource_uri(value)
            {
                resources.push(McpResourceMeta {
                    server: server_name.to_string(),
                    uri,
                });
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

fn parse_read_resource_result(result: &Value) -> String {
    if let Some(content) = parse_content_from_json(result) {
        return content;
    }

    result.to_string()
}

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
