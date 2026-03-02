use std::fs;
use std::path::PathBuf;

use crate::core::agent_config::{McpConfig, McpServerConfig};

use super::client::{LocalMcpClient, McpClient, McpResourceMeta};
use super::config::{summarize_mcp_servers, validate_mcp_config};
use super::context::matched_servers;
use super::{build_mcp_hints, collect_mcp_context};

fn server(name: &str, command: &str, tags: &[&str]) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        command: command.to_string(),
        args: Vec::new(),
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
    assert!(summary.contains("name=demo"));
    assert!(summary.contains("command=npx"));
}

#[test]
fn validate_mcp_config_rejects_invalid_server() {
    let config = McpConfig {
        enabled: true,
        timeout_ms: 500,
        servers: vec![McpServerConfig {
            name: "bad".to_string(),
            command: String::new(),
            args: Vec::new(),
            enabled: true,
            tags: Vec::new(),
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
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("tiangong-mcp-{nonce}.sh"));
    fs::write(&path, content).expect("write test script");
    let mut perms = fs::metadata(&path).expect("stat test script").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod test script");
    TempScript { path }
}

#[cfg(unix)]
#[test]
fn local_mcp_client_command_mode_supports_list_and_read() {
    let script = create_script(
        r#"#!/usr/bin/env bash
set -euo pipefail
action="${1:-}"
if [ "$action" = "list-resources" ]; then
  echo '{"resources":[{"uri":"mcp://demo/first"},{"uri":"mcp://demo/second"}]}'
  exit 0
fi
if [ "$action" = "read-resource" ]; then
  uri="${2:-}"
  if [ "$uri" = "mcp://demo/first" ]; then
    echo '{"content":"hello-from-first"}'
    exit 0
  fi
  echo "read failed" >&2
  exit 5
fi
echo "unknown action" >&2
exit 9
"#,
    );

    let client = LocalMcpClient;
    let server = McpServerConfig {
        name: "demo".to_string(),
        command: script.path.to_string_lossy().to_string(),
        args: Vec::new(),
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
fn local_mcp_client_command_mode_respects_timeout() {
    let script = create_script(
        r#"#!/usr/bin/env bash
set -euo pipefail
sleep 2
echo '{"resources":[{"uri":"mcp://demo/slow"}]}'
"#,
    );

    let client = LocalMcpClient;
    let server = McpServerConfig {
        name: "slow".to_string(),
        command: script.path.to_string_lossy().to_string(),
        args: Vec::new(),
        enabled: true,
        tags: vec!["slow".to_string()],
    };

    let result = client.list_resources(&server, 100);
    assert!(result.is_err());
    let detail = result.err().map(|err| err.to_string()).unwrap_or_default();
    assert!(detail.contains("超时"));
}

#[cfg(unix)]
struct TempDir {
    path: PathBuf,
}

#[cfg(unix)]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn create_temp_dir() -> TempDir {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("tiangong-mcp-dir-{nonce}"));
    fs::create_dir_all(&path).expect("create test dir");
    TempDir { path }
}

#[cfg(unix)]
#[test]
fn local_mcp_client_filesystem_adapter_supports_list_and_read() {
    let temp = create_temp_dir();
    let child = temp.path.join("sample.txt");
    fs::write(&child, "hello adapter").expect("write sample file");

    let client = LocalMcpClient;
    let server = McpServerConfig {
        name: "fs-adapter".to_string(),
        command: "npx".to_string(),
        args: vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
            temp.path.to_string_lossy().to_string(),
        ],
        enabled: true,
        tags: vec!["filesystem".to_string()],
    };

    let resources = client
        .list_resources(&server, 1000)
        .expect("list resources with filesystem adapter");
    assert!(!resources.is_empty());
    assert!(resources[0].uri.starts_with("file://"));

    let content = client
        .read_resource(&server, &resources[0], 1000)
        .expect("read resource with filesystem adapter");
    assert!(content.contains("directory="));
    assert!(content.contains("sample.txt"));
}

#[cfg(unix)]
#[test]
fn local_mcp_client_filesystem_adapter_rejects_outside_root() {
    let root = create_temp_dir();
    let outside = create_temp_dir();
    let outside_file = outside.path.join("outside.txt");
    fs::write(&outside_file, "outside").expect("write outside file");

    let client = LocalMcpClient;
    let server = McpServerConfig {
        name: "fs-adapter".to_string(),
        command: "npx".to_string(),
        args: vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
            root.path.to_string_lossy().to_string(),
        ],
        enabled: true,
        tags: vec!["filesystem".to_string()],
    };

    let resource = McpResourceMeta {
        server: "fs-adapter".to_string(),
        uri: format!("file://{}", outside_file.display()),
    };
    let result = client.read_resource(&server, &resource, 1000);
    assert!(result.is_err());
    let detail = result.err().map(|err| err.to_string()).unwrap_or_default();
    assert!(detail.contains("越界"));
}

#[cfg(unix)]
#[test]
fn collect_mcp_context_uses_filesystem_adapter() {
    let temp = create_temp_dir();
    fs::write(temp.path.join("chain.txt"), "chain").expect("write chain file");

    let config = McpConfig {
        enabled: true,
        timeout_ms: 1000,
        servers: vec![McpServerConfig {
            name: "public-fs-test".to_string(),
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                temp.path.to_string_lossy().to_string(),
            ],
            enabled: true,
            tags: vec!["filesystem".to_string()],
        }],
    };

    let context = collect_mcp_context("请使用 filesystem mcp 查看目录", &config);
    assert!(!context.is_empty());
    assert!(context[0].contains("mcp|ok|server=public-fs-test"));
    assert!(context.iter().any(|item| item.contains("chain.txt")));
}
