use std::collections::{BTreeMap, HashSet};
use std::fs;

use anyhow::{Context, anyhow};
use serde::Deserialize;
use serde_json::Value;

use crate::core::agent_config::{McpConfig, McpServerConfig, McpTransportMode};
use crate::core::app_state::{RegisterMcpServerOptions, TiangongState};
use crate::core::mcp::validate_mcp_config;

use super::args::{McpArgs, McpSubcommand};

pub(super) fn run_mcp_command(args: McpArgs) -> anyhow::Result<()> {
    let mut state = TiangongState::load_or_default();
    match args.command {
        McpSubcommand::List => {
            println!("{}", state.mcp_server_summary(None));
        }
        McpSubcommand::Show { name } => {
            if let Some(name) = name {
                println!("{}", state.mcp_server_detail(Some(&name)));
            } else {
                println!("{}", state.mcp_server_summary(None));
            }
        }
        McpSubcommand::Add {
            name,
            command,
            json,
            json_file,
            force,
            args,
            tags,
            transport,
            endpoint,
            auth_header,
            headers,
            env,
            cwd,
            cmdline,
            enabled,
        } => {
            if json.is_some() || json_file.is_some() {
                ensure_no_mixed_json_add_args(
                    &name,
                    &command,
                    &args,
                    &tags,
                    &transport,
                    &endpoint,
                    &auth_header,
                    &headers,
                    &env,
                    &cwd,
                    &cmdline,
                )?;
                let raw_json = load_add_json_payload(json, json_file)?;
                let imported = parse_mcp_servers_from_json(&raw_json)?;
                let msgs = import_mcp_servers(&mut state, imported, force)?;
                for msg in msgs {
                    println!("{msg}");
                }
            } else {
                let name = name.ok_or_else(|| anyhow!("缺少 MCP server 名称"))?;
                let (command, args) = resolve_mcp_add_command(command, args, cmdline)?;
                if force
                    && state
                        .mcp_servers()
                        .iter()
                        .any(|item| item.name == name.trim())
                {
                    let _ = state.remove_mcp_server(&name)?;
                }
                let msg = state.register_mcp_server(
                    &name,
                    &command,
                    args,
                    tags,
                    enabled,
                    RegisterMcpServerOptions {
                        transport: transport.map(Into::into),
                        endpoint,
                        auth_header,
                        headers,
                        env,
                        cwd,
                    },
                )?;
                println!("{msg}");
            }
        }
        McpSubcommand::Remove { name } => {
            let msg = state.remove_mcp_server(&name)?;
            println!("{msg}");
        }
        McpSubcommand::Enable { name } => {
            let msg = state.set_mcp_server_enabled(&name, true)?;
            println!("{msg}");
        }
        McpSubcommand::Disable { name } => {
            let msg = state.set_mcp_server_enabled(&name, false)?;
            println!("{msg}");
        }
    }
    Ok(())
}

fn resolve_mcp_add_command(
    command: Option<String>,
    mut args: Vec<String>,
    cmdline: Vec<String>,
) -> anyhow::Result<(String, Vec<String>)> {
    let command = command
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut cmdline = cmdline
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    if cmdline.is_empty() {
        return Ok((command.unwrap_or_default(), args));
    }

    if let Some(command) = command {
        args.extend(cmdline);
        return Ok((command, args));
    }

    if cmdline.first().is_some_and(|item| item.starts_with('-')) {
        return Err(anyhow::anyhow!(
            "命令缺少可执行项，请在 -- 后提供 command，如 -- npx -y ..."
        ));
    }

    let command = cmdline.remove(0);
    args.extend(cmdline);
    Ok((command, args))
}

#[allow(clippy::too_many_arguments)]
fn ensure_no_mixed_json_add_args(
    name: &Option<String>,
    command: &Option<String>,
    args: &[String],
    tags: &[String],
    transport: &Option<super::args::McpTransportArg>,
    endpoint: &Option<String>,
    auth_header: &Option<String>,
    headers: &[(String, String)],
    env: &[(String, String)],
    cwd: &Option<String>,
    cmdline: &[String],
) -> anyhow::Result<()> {
    if name.as_ref().is_some_and(|value| !value.trim().is_empty())
        || command
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || !args.is_empty()
        || !tags.is_empty()
        || transport.is_some()
        || endpoint
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || auth_header
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
        || !headers.is_empty()
        || !env.is_empty()
        || cwd.as_ref().is_some_and(|value| !value.trim().is_empty())
        || !cmdline.is_empty()
    {
        return Err(anyhow!(
            "JSON 导入模式下不允许同时传 name/command/arg/tags/transport/endpoint/auth/header/env/cwd/CMDLINE 参数"
        ));
    }
    Ok(())
}

fn load_add_json_payload(
    json: Option<String>,
    json_file: Option<String>,
) -> anyhow::Result<String> {
    if let Some(raw) = json {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(anyhow!("--json 不能为空"));
        }
        return Ok(raw.to_string());
    }

    let Some(path) = json_file else {
        return Err(anyhow!("请提供 --json 或 --json-file"));
    };
    let path = path.trim();
    if path.is_empty() {
        return Err(anyhow!("--json-file 路径不能为空"));
    }
    fs::read_to_string(path).with_context(|| format!("读取 JSON 文件失败：{path}"))
}

#[derive(Debug, Clone)]
struct ImportedMcpServer {
    name: String,
    command: String,
    args: Vec<String>,
    tags: Vec<String>,
    enabled: bool,
    options: RegisterMcpServerOptions,
}

#[derive(Debug, Deserialize, Default)]
struct JsonMcpServerInput {
    #[serde(default)]
    name: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    transport: String,
    #[serde(default, rename = "type")]
    mcp_type: String,
    #[serde(default, alias = "authHeader")]
    auth_header: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    tags: Vec<String>,
}

fn parse_mcp_servers_from_json(raw_json: &str) -> anyhow::Result<Vec<ImportedMcpServer>> {
    let value = serde_json::from_str::<Value>(raw_json).context("解析 JSON 失败")?;
    let Value::Object(map) = value else {
        return Err(anyhow!(
            "JSON 顶层必须为对象，支持单个 server 对象或包含 mcpServers 的对象"
        ));
    };

    if let Some(mcp_servers_value) = map.get("mcpServers") {
        parse_mcp_servers_map(mcp_servers_value)
    } else {
        let input: JsonMcpServerInput =
            serde_json::from_value(Value::Object(map)).context("解析单个 MCP server JSON 失败")?;
        Ok(vec![normalize_json_server(input, None)?])
    }
}

fn parse_mcp_servers_map(value: &Value) -> anyhow::Result<Vec<ImportedMcpServer>> {
    let map = serde_json::from_value::<BTreeMap<String, JsonMcpServerInput>>(value.clone())
        .context("mcpServers 必须是对象，格式如 {\"mcpServers\": {\"name\": {...}}}")?;
    if map.is_empty() {
        return Err(anyhow!("mcpServers 不能为空"));
    }

    map.into_iter()
        .map(|(key, input)| normalize_json_server(input, Some(key)))
        .collect()
}

fn normalize_json_server(
    input: JsonMcpServerInput,
    map_key_name: Option<String>,
) -> anyhow::Result<ImportedMcpServer> {
    let map_key_name = map_key_name.unwrap_or_default();
    let map_key_name_trimmed = map_key_name.trim().to_string();
    let explicit_name = input.name.trim().to_string();

    if !explicit_name.is_empty()
        && !map_key_name_trimmed.is_empty()
        && explicit_name != map_key_name_trimmed
    {
        return Err(anyhow!(
            "mcpServers.{}.name 与 key 不一致：{} != {}",
            map_key_name_trimmed,
            explicit_name,
            map_key_name_trimmed
        ));
    }

    let name = if explicit_name.is_empty() {
        map_key_name_trimmed
    } else {
        explicit_name
    };
    if name.is_empty() {
        return Err(anyhow!("导入 MCP server 失败：缺少 name"));
    }

    let transport = parse_transport_mode(&input.transport, &input.mcp_type)?;
    let endpoint = if !input.endpoint.trim().is_empty() {
        input.endpoint.trim().to_string()
    } else {
        input.url.trim().to_string()
    };
    let command = input.command.trim().to_string();
    let args = input
        .args
        .into_iter()
        .map(|arg| arg.trim().to_string())
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>();
    let tags = input
        .tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    let headers = normalize_map_pairs(input.headers);
    let env = normalize_map_pairs(input.env);
    let auth_header = input.auth_header.trim().to_string();
    let cwd = input.cwd.trim().to_string();

    Ok(ImportedMcpServer {
        name,
        command,
        args,
        tags,
        enabled: input.enabled,
        options: RegisterMcpServerOptions {
            transport: Some(transport),
            endpoint: Some(endpoint),
            auth_header: Some(auth_header),
            headers,
            env,
            cwd: Some(cwd),
        },
    })
}

fn parse_transport_mode(transport: &str, mcp_type: &str) -> anyhow::Result<McpTransportMode> {
    let candidate = if !transport.trim().is_empty() {
        transport.trim()
    } else {
        mcp_type.trim()
    };
    if candidate.is_empty() {
        return Ok(McpTransportMode::Auto);
    }

    match candidate.to_ascii_lowercase().as_str() {
        "auto" => Ok(McpTransportMode::Auto),
        "stdio" => Ok(McpTransportMode::Stdio),
        "http" | "streamablehttp" | "streamable_http" | "streamable-http" => {
            Ok(McpTransportMode::Http)
        }
        other => Err(anyhow!(
            "不支持的 transport/type：{other}，支持 auto/stdio/http/streamableHttp"
        )),
    }
}

fn normalize_map_pairs(input: BTreeMap<String, String>) -> Vec<(String, String)> {
    input
        .into_iter()
        .filter_map(|(key, value)| {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if key.is_empty() || value.is_empty() {
                None
            } else {
                Some((key, value))
            }
        })
        .collect()
}

fn import_mcp_servers(
    state: &mut TiangongState,
    servers: Vec<ImportedMcpServer>,
    force: bool,
) -> anyhow::Result<Vec<String>> {
    if servers.is_empty() {
        return Err(anyhow!("没有可导入的 MCP server"));
    }

    ensure_import_servers_valid(&servers)?;
    ensure_import_conflicts(state, &servers, force)?;

    let mut msgs = Vec::new();
    for server in servers {
        let exists = state
            .mcp_servers()
            .iter()
            .any(|item| item.name == server.name);
        if force && exists {
            let msg = state.remove_mcp_server(&server.name)?;
            msgs.push(msg);
        }

        let msg = state.register_mcp_server(
            &server.name,
            &server.command,
            server.args,
            server.tags,
            server.enabled,
            server.options,
        )?;
        msgs.push(msg);
    }

    Ok(msgs)
}

fn ensure_import_servers_valid(servers: &[ImportedMcpServer]) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for server in servers {
        if !seen.insert(server.name.clone()) {
            return Err(anyhow!(
                "导入 JSON 包含重复 MCP server 名称：{}",
                server.name
            ));
        }
    }

    let config = McpConfig {
        enabled: true,
        timeout_ms: 15_000,
        servers: servers
            .iter()
            .map(imported_server_to_config)
            .collect::<Vec<McpServerConfig>>(),
    };
    validate_mcp_config(&config)?;
    Ok(())
}

fn ensure_import_conflicts(
    state: &TiangongState,
    servers: &[ImportedMcpServer],
    force: bool,
) -> anyhow::Result<()> {
    if force {
        return Ok(());
    }

    let existing = state
        .mcp_servers()
        .iter()
        .map(|item| item.name.as_str())
        .collect::<HashSet<_>>();
    let conflicts = servers
        .iter()
        .filter(|server| existing.contains(server.name.as_str()))
        .map(|server| server.name.clone())
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        return Err(anyhow!(
            "导入失败，存在同名 MCP server：{}；如需覆盖请添加 --force",
            conflicts.join(", ")
        ));
    }

    Ok(())
}

fn imported_server_to_config(server: &ImportedMcpServer) -> McpServerConfig {
    let endpoint = server.options.endpoint.clone().unwrap_or_default();
    let auth_header = server.options.auth_header.clone().unwrap_or_default();
    let cwd = server.options.cwd.clone().unwrap_or_default();
    let headers = server
        .options
        .headers
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();
    let env = server
        .options
        .env
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();

    McpServerConfig {
        name: server.name.clone(),
        transport: server.options.transport.clone().unwrap_or_default(),
        command: server.command.clone(),
        args: server.args.clone(),
        endpoint,
        auth_header,
        headers,
        env,
        cwd,
        enabled: server.enabled,
        tags: server.tags.clone(),
    }
}

fn default_enabled() -> bool {
    true
}
