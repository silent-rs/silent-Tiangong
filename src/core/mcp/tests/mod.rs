use std::fs;
use std::path::PathBuf;

use crate::core::agent_config::{McpConfig, McpServerConfig, McpTransportMode};

use super::client::{LocalMcpClient, McpClient};
use super::config::{describe_mcp_servers, summarize_mcp_servers, validate_mcp_config};
use super::context::matched_servers;
use super::{build_mcp_hints, collect_mcp_context};

fn server(name: &str, command: &str, tags: &[&str]) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransportMode::Auto,
        command: command.to_string(),
        args: Vec::new(),
        endpoint: String::new(),
        auth_header: String::new(),
        headers: Default::default(),
        env: Default::default(),
        cwd: String::new(),
        enabled: true,
        tags: tags.iter().map(|item| item.to_string()).collect(),
    }
}

fn config_with_servers(servers: Vec<McpServerConfig>) -> McpConfig {
    McpConfig {
        enabled: true,
        timeout_ms: 500,
        servers,
    }
}

#[test]
fn build_mcp_hints_returns_skipped_when_disabled() {
    let config = McpConfig {
        enabled: false,
        timeout_ms: 1000,
        servers: vec![server("browser", "", &["网页"])],
    };
    let hints = build_mcp_hints("打开网页", &config);
    assert_eq!(
        hints,
        vec!["mcp|skipped|server=all|detail=mcp disabled or empty"]
    );
}

#[test]
fn build_mcp_hints_returns_skipped_when_no_server_matched() {
    let config = config_with_servers(vec![server("browser", "", &["网页"])]);
    let hints = build_mcp_hints("查询订单", &config);
    assert_eq!(
        hints,
        vec!["mcp|skipped|server=all|detail=no matched server"]
    );
}

#[test]
fn matched_servers_supports_keyword_rules() {
    let config = config_with_servers(vec![
        server("browser-hub", "", &["网页"]),
        server("db-main", "", &["数据库"]),
        server("ops", "", &["运维"]),
    ]);

    let browser = matched_servers("请帮我看看这个页面", &config);
    assert_eq!(browser.len(), 1);
    assert_eq!(browser[0].name, "browser-hub");

    let db = matched_servers("查询sql表结构", &config);
    assert_eq!(db.len(), 1);
    assert_eq!(db[0].name, "db-main");
}

#[test]
fn collect_mcp_context_limits_items() {
    let config = config_with_servers(vec![server(
        "browser",
        "",
        &["tag1", "tag2", "tag3", "tag4", "tag5"],
    )]);
    let context = collect_mcp_context("browser", &config);
    assert_eq!(context.len(), 4);
    assert!(
        context
            .iter()
            .all(|item| item.starts_with("mcp|ok|server=browser"))
    );
}

#[test]
fn build_mcp_hints_surfaces_command_failure() {
    let config = config_with_servers(vec![server(
        "failing",
        "/path/does-not-exist/tiangong-mcp",
        &["failing"],
    )]);
    let hints = build_mcp_hints("failing", &config);
    assert_eq!(hints.len(), 1);
    assert!(
        hints[0].starts_with("mcp|error|server=failing|detail=timeout_ms=500,action=list,error=")
    );
}

#[test]
fn summarize_mcp_servers_works() {
    let summary =
        summarize_mcp_servers(&[server("demo", "npx", &["web", "browser"])], Some("demo"));
    assert!(summary.contains("demo"));
    assert!(summary.contains("\u{1b}[32m"));
    assert!(!summary.contains("command"));
}

#[test]
fn describe_mcp_servers_works() {
    let detail = describe_mcp_servers(&[server("demo", "npx", &["web"])], Some("demo"));
    assert!(detail.contains("mcp name: demo"));
    assert!(detail.contains("\u{1b}[32m"));
    assert!(!detail.contains("1. demo"));
    assert!(detail.contains("command: npx"));
    assert!(detail.contains("transport: stdio"));
}

#[test]
fn validate_mcp_config_rejects_invalid_server() {
    let config = McpConfig {
        enabled: true,
        timeout_ms: 500,
        servers: vec![McpServerConfig {
            name: "bad".to_string(),
            transport: McpTransportMode::Auto,
            command: String::new(),
            args: Vec::new(),
            endpoint: String::new(),
            auth_header: String::new(),
            headers: Default::default(),
            env: Default::default(),
            cwd: String::new(),
            enabled: true,
            tags: Vec::new(),
        }],
    };
    let result = validate_mcp_config(&config);
    assert!(result.is_err());
}

#[test]
fn validate_mcp_config_accepts_http_server_with_endpoint() {
    let config = McpConfig {
        enabled: true,
        timeout_ms: 500,
        servers: vec![McpServerConfig {
            name: "remote".to_string(),
            transport: McpTransportMode::Http,
            command: String::new(),
            args: Vec::new(),
            endpoint: "https://example.com/mcp".to_string(),
            auth_header: "token".to_string(),
            headers: Default::default(),
            env: Default::default(),
            cwd: String::new(),
            enabled: true,
            tags: vec!["remote".to_string()],
        }],
    };
    let result = validate_mcp_config(&config);
    assert!(result.is_ok());
}

#[test]
fn validate_mcp_config_rejects_http_server_with_args() {
    let config = McpConfig {
        enabled: true,
        timeout_ms: 500,
        servers: vec![McpServerConfig {
            name: "remote".to_string(),
            transport: McpTransportMode::Http,
            command: "https://example.com/mcp".to_string(),
            args: vec!["--bad".to_string()],
            endpoint: String::new(),
            auth_header: String::new(),
            headers: Default::default(),
            env: Default::default(),
            cwd: String::new(),
            enabled: true,
            tags: vec!["remote".to_string()],
        }],
    };
    let result = validate_mcp_config(&config);
    assert!(result.is_err());
}

#[cfg(unix)]
struct TempScript {
    path: PathBuf,
}

#[cfg(unix)]
impl Drop for TempScript {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn create_script(content: &str) -> TempScript {
    use std::os::unix::fs::PermissionsExt;
    let nonce = scru128::new();
    let path = std::env::temp_dir().join(format!("tiangong-mcp-{nonce}.sh"));
    fs::write(&path, content).expect("write test script");
    let mut perms = fs::metadata(&path).expect("stat test script").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod test script");
    TempScript { path }
}

#[cfg(unix)]
#[test]
fn local_mcp_client_stdio_mode_supports_list_and_read() {
    let script = create_script(
        r#"#!/usr/bin/env bash
set -euo pipefail

send_msg() {
  printf '%s\n' "$1"
}

extract_id() {
  local payload="$1"
  local id
  id="$(printf '%s' "$payload" | sed -n 's/.*"id":[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)"
  if [ -z "$id" ]; then
    id=1
  fi
  printf '%s' "$id"
}

while true; do
  IFS= read -r payload || exit 0
  payload="${payload%$'\r'}"
  [ -z "$payload" ] && continue
  req_id="$(extract_id "$payload")"

  if [[ "$payload" == *'"method":"initialize"'* ]]; then
    send_msg "{\"jsonrpc\":\"2.0\",\"id\":${req_id},\"result\":{\"protocolVersion\":\"2025-11-05\",\"capabilities\":{\"resources\":{}},\"serverInfo\":{\"name\":\"demo\",\"version\":\"1.0.0\"}}}"
    continue
  fi

  if [[ "$payload" == *'"method":"notifications/initialized"'* ]]; then
    continue
  fi

  if [[ "$payload" == *'"method":"resources/list"'* ]]; then
    send_msg "{\"jsonrpc\":\"2.0\",\"id\":${req_id},\"result\":{\"resources\":[{\"uri\":\"mcp://demo/first\",\"name\":\"first\"},{\"uri\":\"mcp://demo/second\",\"name\":\"second\"}]}}"
    continue
  fi

  if [[ "$payload" == *'"method":"resources/read"'* && "$payload" == *'mcp://demo/first'* ]]; then
    send_msg "{\"jsonrpc\":\"2.0\",\"id\":${req_id},\"result\":{\"contents\":[{\"uri\":\"mcp://demo/first\",\"text\":\"hello-from-first\"}]}}"
    continue
  fi

  if [[ "$payload" == *'"method":"resources/read"'* ]]; then
    send_msg "{\"jsonrpc\":\"2.0\",\"id\":${req_id},\"error\":{\"code\":-32002,\"message\":\"read failed\"}}"
    continue
  fi

done
"#,
    );

    let client = LocalMcpClient;
    let server = McpServerConfig {
        name: "demo".to_string(),
        transport: McpTransportMode::Auto,
        command: script.path.to_string_lossy().to_string(),
        args: Vec::new(),
        endpoint: String::new(),
        auth_header: String::new(),
        headers: Default::default(),
        env: Default::default(),
        cwd: String::new(),
        enabled: true,
        tags: vec!["demo".to_string()],
    };

    let resources = client
        .list_resources(&server, 1000)
        .expect("list resources");
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0].uri, "mcp://demo/first");

    let first_content = client
        .read_resource(&server, &resources[0], 1000)
        .expect("read first resource");
    assert_eq!(first_content, "hello-from-first");

    let read_err = client.read_resource(&server, &resources[1], 1000);
    assert!(read_err.is_err());
}

#[cfg(unix)]
#[test]
fn local_mcp_client_stdio_mode_respects_timeout() {
    let script = create_script(
        r#"#!/usr/bin/env bash
set -euo pipefail

send_msg() {
  printf '%s\n' "$1"
}

extract_id() {
  local payload="$1"
  local id
  id="$(printf '%s' "$payload" | sed -n 's/.*"id":[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)"
  if [ -z "$id" ]; then
    id=1
  fi
  printf '%s' "$id"
}

while true; do
  IFS= read -r payload || exit 0
  payload="${payload%$'\r'}"
  [ -z "$payload" ] && continue
  req_id="$(extract_id "$payload")"

  if [[ "$payload" == *'"method":"initialize"'* ]]; then
    sleep 2
    send_msg "{\"jsonrpc\":\"2.0\",\"id\":${req_id},\"result\":{\"protocolVersion\":\"2025-11-05\",\"capabilities\":{},\"serverInfo\":{\"name\":\"slow\",\"version\":\"1.0.0\"}}}"
    continue
  fi

done
"#,
    );

    let client = LocalMcpClient;
    let server = McpServerConfig {
        name: "slow".to_string(),
        transport: McpTransportMode::Auto,
        command: script.path.to_string_lossy().to_string(),
        args: Vec::new(),
        endpoint: String::new(),
        auth_header: String::new(),
        headers: Default::default(),
        env: Default::default(),
        cwd: String::new(),
        enabled: true,
        tags: vec!["slow".to_string()],
    };

    let result = client.list_resources(&server, 100);
    assert!(result.is_err());
    let detail = result.err().map(|err| err.to_string()).unwrap_or_default();
    assert!(detail.contains("超时"));
}

#[cfg(unix)]
#[test]
fn collect_mcp_context_uses_stdio_mcp_server() {
    let script = create_script(
        r#"#!/usr/bin/env bash
set -euo pipefail

send_msg() {
  printf '%s\n' "$1"
}

extract_id() {
  local payload="$1"
  local id
  id="$(printf '%s' "$payload" | sed -n 's/.*"id":[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)"
  if [ -z "$id" ]; then
    id=1
  fi
  printf '%s' "$id"
}

while true; do
  IFS= read -r payload || exit 0
  payload="${payload%$'\r'}"
  [ -z "$payload" ] && continue
  req_id="$(extract_id "$payload")"

  if [[ "$payload" == *'"method":"initialize"'* ]]; then
    send_msg "{\"jsonrpc\":\"2.0\",\"id\":${req_id},\"result\":{\"protocolVersion\":\"2025-11-05\",\"capabilities\":{\"resources\":{}},\"serverInfo\":{\"name\":\"public-fs-test\",\"version\":\"1.0.0\"}}}"
    continue
  fi

  if [[ "$payload" == *'"method":"notifications/initialized"'* ]]; then
    continue
  fi

  if [[ "$payload" == *'"method":"resources/list"'* ]]; then
    send_msg "{\"jsonrpc\":\"2.0\",\"id\":${req_id},\"result\":{\"resources\":[{\"uri\":\"mcp://public-fs-test/chain\",\"name\":\"chain\"}]}}"
    continue
  fi

  if [[ "$payload" == *'"method":"resources/read"'* ]]; then
    send_msg "{\"jsonrpc\":\"2.0\",\"id\":${req_id},\"result\":{\"contents\":[{\"uri\":\"mcp://public-fs-test/chain\",\"text\":\"chain.txt\"}]}}"
    continue
  fi

done
"#,
    );

    let config = McpConfig {
        enabled: true,
        timeout_ms: 1000,
        servers: vec![McpServerConfig {
            name: "public-fs-test".to_string(),
            transport: McpTransportMode::Auto,
            command: script.path.to_string_lossy().to_string(),
            args: Vec::new(),
            endpoint: String::new(),
            auth_header: String::new(),
            headers: Default::default(),
            env: Default::default(),
            cwd: String::new(),
            enabled: true,
            tags: vec!["filesystem".to_string()],
        }],
    };

    let context = collect_mcp_context("请使用 filesystem mcp 查看目录", &config);
    assert!(!context.is_empty());
    assert!(
        context
            .iter()
            .any(|item| item.contains("mcp|ok|server=public-fs-test"))
    );
    assert!(context.iter().any(|item| item.contains("chain.txt")));
}
