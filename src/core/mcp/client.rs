use std::collections::HashSet;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use crate::core::agent_config::McpServerConfig;

use super::util::truncate_text;

const MCP_PROTOCOL_VERSION: &str = "2025-11-05";
const MCP_CLIENT_NAME: &str = "tiangong";
const MAX_LIST_PAGES: usize = 8;
const INIT_REQUEST_ID: u64 = 1;
const METHOD_REQUEST_ID: u64 = 2;

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

        if server.command.trim().is_empty() {
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

        list_resources_via_stdio(server, timeout_ms)
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

        if server.command.trim().is_empty() {
            return Ok(format!(
                "resource={} from_server={} command={} args={}",
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

fn list_resources_via_stdio(
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
    let command = server.command.trim();
    if command.is_empty() {
        return Err(anyhow!("MCP server command 为空：{}", server.name));
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
    let command = server.command.trim();
    let mut cmd = Command::new(command);
    cmd.kill_on_drop(true)
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().with_context(|| {
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

    let mut stdin = child.stdin.take().context("MCP进程 stdin 管道不可用")?;
    let stdout = child.stdout.take().context("MCP进程 stdout 管道不可用")?;
    let stderr = child.stderr.take().context("MCP进程 stderr 管道不可用")?;

    let stderr_task = tokio::spawn(async move {
        let mut stderr = BufReader::new(stderr);
        let mut buffer = String::new();
        let _ = stderr.read_to_string(&mut buffer).await;
        buffer
    });

    let mut stdout = BufReader::new(stdout);

    let request_result = async {
        send_jsonrpc_request(
            &mut stdin,
            INIT_REQUEST_ID,
            "initialize",
            Some(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": MCP_CLIENT_NAME,
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
        )
        .await?;
        let _ = read_jsonrpc_result(&mut stdout, INIT_REQUEST_ID).await?;

        send_jsonrpc_notification(&mut stdin, "notifications/initialized", Some(json!({}))).await?;

        send_jsonrpc_request(&mut stdin, METHOD_REQUEST_ID, method, params).await?;
        read_jsonrpc_result(&mut stdout, METHOD_REQUEST_ID).await
    }
    .await;

    drop(stdin);
    shutdown_child(&mut child).await;

    let stderr = stderr_task.await.unwrap_or_default();
    match request_result {
        Ok(result) => Ok(result),
        Err(err) => {
            let stderr = stderr.trim();
            if stderr.is_empty() {
                Err(err)
            } else {
                Err(anyhow!("{}; stderr={}", err, truncate_text(stderr, 200)))
            }
        }
    }
}

async fn shutdown_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

async fn send_jsonrpc_request(
    stdin: &mut ChildStdin,
    id: u64,
    method: &str,
    params: Option<Value>,
) -> Result<()> {
    let mut message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
    });
    if let Some(params) = params
        && let Some(object) = message.as_object_mut()
    {
        object.insert("params".to_string(), params);
    }
    send_jsonrpc_message(stdin, &message).await
}

async fn send_jsonrpc_notification(
    stdin: &mut ChildStdin,
    method: &str,
    params: Option<Value>,
) -> Result<()> {
    let mut message = json!({
        "jsonrpc": "2.0",
        "method": method,
    });
    if let Some(params) = params
        && let Some(object) = message.as_object_mut()
    {
        object.insert("params".to_string(), params);
    }
    send_jsonrpc_message(stdin, &message).await
}

async fn send_jsonrpc_message(stdin: &mut ChildStdin, message: &Value) -> Result<()> {
    let payload = serde_json::to_vec(message).context("序列化 MCP JSON-RPC 消息失败")?;
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    stdin
        .write_all(header.as_bytes())
        .await
        .context("写入 MCP 消息头失败")?;
    stdin
        .write_all(&payload)
        .await
        .context("写入 MCP 消息体失败")?;
    stdin.flush().await.context("刷新 MCP stdin 失败")?;
    Ok(())
}

async fn read_jsonrpc_result(
    stdout: &mut BufReader<ChildStdout>,
    expected_id: u64,
) -> Result<Value> {
    loop {
        let message = read_jsonrpc_message(stdout).await?;
        let object = match message.as_object() {
            Some(object) => object,
            None => continue,
        };

        let Some(id) = object.get("id") else {
            continue;
        };
        if !matches_request_id(id, expected_id) {
            continue;
        }

        if let Some(error) = object.get("error") {
            return Err(anyhow!("MCP响应错误：{}", format_jsonrpc_error(error)));
        }

        if let Some(result) = object.get("result") {
            return Ok(result.clone());
        }

        return Err(anyhow!("MCP响应缺少 result 字段"));
    }
}

fn matches_request_id(id: &Value, expected_id: u64) -> bool {
    match id {
        Value::Number(number) => number.as_u64() == Some(expected_id),
        Value::String(text) => text.parse::<u64>().ok() == Some(expected_id),
        _ => false,
    }
}

async fn read_jsonrpc_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value> {
    loop {
        let mut line = String::new();
        let read = stdout
            .read_line(&mut line)
            .await
            .context("读取 MCP 响应失败")?;
        if read == 0 {
            return Err(anyhow!("MCP stdout 已关闭"));
        }

        let line = line.trim_end_matches(['\r', '\n']).trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('{') || line.starts_with('[') {
            return parse_json_message(line);
        }

        let mut content_length = parse_content_length_header(line);
        let mut has_headers = line.contains(':');

        loop {
            let mut header_line = String::new();
            let read = stdout
                .read_line(&mut header_line)
                .await
                .context("读取 MCP 响应头失败")?;
            if read == 0 {
                return Err(anyhow!("MCP 响应头提前结束"));
            }

            let header_line = header_line.trim_end_matches(['\r', '\n']);
            if header_line.is_empty() {
                break;
            }

            has_headers = true;
            if let Some(length) = parse_content_length_header(header_line) {
                content_length = Some(length);
            }
        }

        if let Some(content_length) = content_length {
            let mut payload = vec![0_u8; content_length];
            stdout
                .read_exact(&mut payload)
                .await
                .context("读取 MCP 响应体失败")?;
            let payload = String::from_utf8(payload).context("MCP 响应体不是 UTF-8")?;
            return parse_json_message(payload.trim());
        }

        if has_headers {
            return Err(anyhow!("MCP 响应头缺少 Content-Length"));
        }

        return parse_json_message(line);
    }
}

fn parse_content_length_header(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    if name.trim().eq_ignore_ascii_case("Content-Length") {
        return value.trim().parse::<usize>().ok();
    }
    None
}

fn parse_json_message(payload: &str) -> Result<Value> {
    serde_json::from_str::<Value>(payload)
        .with_context(|| format!("解析 MCP JSON 消息失败：{}", truncate_text(payload, 200)))
}

fn format_jsonrpc_error(error: &Value) -> String {
    if let Value::Object(error) = error {
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("(empty)")
            .trim();
        let data = error
            .get("data")
            .map(|value| truncate_text(&value.to_string(), 120))
            .unwrap_or_else(|| "(none)".to_string());
        return format!("code={} message={} data={}", code, message, data);
    }

    truncate_text(&error.to_string(), 200)
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
