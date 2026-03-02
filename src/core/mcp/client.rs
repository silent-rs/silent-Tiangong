use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tokio::process::Command;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use crate::core::agent_config::McpServerConfig;

use super::util::truncate_text;

const FILESYSTEM_MCP_PACKAGE: &str = "@modelcontextprotocol/server-filesystem";
const FILE_URI_PREFIX: &str = "file://";

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

        if let Some(roots) = filesystem_roots_from_server(server) {
            return list_filesystem_resources(server, &roots);
        }

        let output = run_mcp_command(server, timeout_ms, "list-resources", None)?;
        parse_resource_list_output(server, &output)
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

        if let Some(roots) = filesystem_roots_from_server(server) {
            return read_filesystem_resource(server, resource, &roots);
        }

        let output = run_mcp_command(server, timeout_ms, "read-resource", Some(&resource.uri))?;
        Ok(parse_read_resource_output(&output))
    }
}

fn filesystem_roots_from_server(server: &McpServerConfig) -> Option<Vec<PathBuf>> {
    let command = server.command.trim().to_ascii_lowercase();
    if command != "npx" && command != "npx.cmd" {
        return None;
    }

    let pkg_idx = server
        .args
        .iter()
        .position(|arg| arg.trim() == FILESYSTEM_MCP_PACKAGE)?;

    let cwd = std::env::current_dir().ok();
    let mut roots = Vec::new();
    for raw in server.args.iter().skip(pkg_idx + 1) {
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('-') {
            continue;
        }
        let candidate = PathBuf::from(raw);
        let resolved = if candidate.is_absolute() {
            candidate
        } else if let Some(base) = cwd.as_ref() {
            base.join(candidate)
        } else {
            candidate
        };
        let canonical = resolved.canonicalize().unwrap_or(resolved);
        if !roots.iter().any(|root| root == &canonical) {
            roots.push(canonical);
        }
    }

    if roots.is_empty() { None } else { Some(roots) }
}

fn list_filesystem_resources(
    server: &McpServerConfig,
    roots: &[PathBuf],
) -> Result<Vec<McpResourceMeta>> {
    let mut resources = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        resources.push(McpResourceMeta {
            server: server.name.clone(),
            uri: format!("{}{}", FILE_URI_PREFIX, root.display()),
        });
    }

    if resources.is_empty() {
        return Err(anyhow!(
            "MCP filesystem server 无可用根目录：{}",
            server.name
        ));
    }
    Ok(resources)
}

fn read_filesystem_resource(
    server: &McpServerConfig,
    resource: &McpResourceMeta,
    roots: &[PathBuf],
) -> Result<String> {
    let raw_path = resource
        .uri
        .strip_prefix(FILE_URI_PREFIX)
        .unwrap_or(resource.uri.as_str());
    let path = PathBuf::from(raw_path);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("MCP filesystem 资源不存在：{}", path.display()))?;

    if !is_path_under_roots(&canonical, roots) {
        return Err(anyhow!(
            "MCP filesystem 资源越界：{}（server={}）",
            canonical.display(),
            server.name
        ));
    }

    if canonical.is_dir() {
        summarize_dir(&canonical)
    } else {
        summarize_file(&canonical)
    }
}

fn is_path_under_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn summarize_dir(path: &Path) -> Result<String> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("读取目录失败：{}", path.display()))?
        .filter_map(|item| item.ok())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    const MAX_ENTRIES: usize = 24;
    let mut lines = vec![format!("directory={}", path.display())];
    for entry in entries.iter().take(MAX_ENTRIES) {
        let entry_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let marker = if entry_path.is_dir() { "dir" } else { "file" };
        lines.push(format!("[{marker}] {name}"));
    }
    if entries.len() > MAX_ENTRIES {
        lines.push(format!("... ({} more)", entries.len() - MAX_ENTRIES));
    }
    Ok(lines.join("\n"))
}

fn summarize_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("读取文件失败：{}", path.display()))?;
    let content = String::from_utf8_lossy(&bytes);
    Ok(format!(
        "file={}\ncontent={}",
        path.display(),
        truncate_text(&content, 400)
    ))
}

fn run_mcp_command(
    server: &McpServerConfig,
    timeout_ms: u64,
    action: &str,
    resource_uri: Option<&str>,
) -> Result<String> {
    let command = server.command.trim();
    if command.is_empty() {
        return Err(anyhow!("MCP server command 为空：{}", server.name));
    }

    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化 MCP 运行时失败")?;
    let output = runtime.block_on(async {
        let mut cmd = Command::new(command);
        cmd.args(&server.args)
            .arg(action)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(uri) = resource_uri {
            cmd.arg(uri);
        }
        timeout(Duration::from_millis(timeout_ms), cmd.output()).await
    });

    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(anyhow!(
                "MCP命令执行失败：server={} command={} error={}",
                server.name,
                command,
                err
            ));
        }
        Err(_) => {
            return Err(anyhow!(
                "MCP命令执行超时：server={} action={} timeout_ms={}",
                server.name,
                action,
                timeout_ms
            ));
        }
    };

    if !output.status.success() {
        let exit_code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stderr = if stderr.is_empty() {
            "(empty)".to_string()
        } else {
            truncate_text(&stderr, 160)
        };
        return Err(anyhow!(
            "MCP命令返回失败：server={} action={} exit_code={} stderr={}",
            server.name,
            action,
            exit_code,
            stderr
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(anyhow!(
            "MCP命令输出为空：server={} action={}",
            server.name,
            action
        ));
    }
    Ok(stdout)
}

fn parse_resource_list_output(
    server: &McpServerConfig,
    output: &str,
) -> Result<Vec<McpResourceMeta>> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
        let resources = parse_resource_list_from_json(&server.name, &json);
        if !resources.is_empty() {
            return Ok(resources);
        }
    }

    let resources = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| McpResourceMeta {
            server: server.name.clone(),
            uri: line.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(resources)
}

fn parse_resource_list_from_json(server_name: &str, value: &Value) -> Vec<McpResourceMeta> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(parse_resource_uri)
            .map(|uri| McpResourceMeta {
                server: server_name.to_string(),
                uri,
            })
            .collect(),
        Value::Object(map) => {
            if let Some(resources) = map.get("resources") {
                return parse_resource_list_from_json(server_name, resources);
            }
            parse_resource_uri(value)
                .map(|uri| {
                    vec![McpResourceMeta {
                        server: server_name.to_string(),
                        uri,
                    }]
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
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

fn parse_read_resource_output(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Ok(json) = serde_json::from_str::<Value>(trimmed)
        && let Some(content) = parse_content_from_json(&json)
    {
        return content;
    }

    trimmed.to_string()
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
            for key in ["content", "text", "data"] {
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
