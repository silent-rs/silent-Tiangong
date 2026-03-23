use super::super::*;
use crate::app_state::audit;

#[derive(Debug, Clone, Copy, Default)]
pub struct AppMcpService;

impl AppMcpService {
    pub(in crate::app_state) fn register_mcp_server(
        self,
        state: &mut TiangongState,
        request: RegisterMcpServerRequest,
    ) -> Result<String> {
        let name = request.name.trim();
        let command = request.command.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }
        if state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .iter()
            .any(|server| server.name == name)
        {
            return Err(anyhow!("MCP server 已存在：{name}"));
        }

        let tags = request
            .tags
            .into_iter()
            .map(|tag| tag.trim().to_string())
            .filter(|tag| !tag.is_empty())
            .collect::<Vec<_>>();
        let args = request
            .args
            .into_iter()
            .map(|arg| arg.trim().to_string())
            .filter(|arg| !arg.is_empty())
            .collect::<Vec<_>>();
        let transport = request.options.transport.unwrap_or_default();
        let endpoint = request
            .options
            .endpoint
            .unwrap_or_default()
            .trim()
            .to_string();
        let auth_header = request
            .options
            .auth_header
            .unwrap_or_default()
            .trim()
            .to_string();
        let cwd = request.options.cwd.unwrap_or_default().trim().to_string();

        let headers = request
            .options
            .headers
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
            .fold(BTreeMap::new(), |mut map, (key, value)| {
                map.insert(key, value);
                map
            });
        let env = request
            .options
            .env
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
            .fold(BTreeMap::new(), |mut map, (key, value)| {
                map.insert(key, value);
                map
            });

        if command.is_empty() && endpoint.is_empty() && tags.is_empty() {
            return Err(anyhow!("MCP server 需至少配置 command、endpoint 或 tags"));
        }

        state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .push(McpServerConfig {
                name: name.to_string(),
                transport,
                command: command.to_string(),
                args,
                endpoint,
                auth_header,
                headers,
                env,
                cwd,
                enabled: request.enabled,
                tags,
            });
        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        audit::append_audit_log(&audit::AuditEntry::new(
            "mcp.register",
            name,
            &format!("MCP server 已注册：{name}"),
            true,
        ));
        Ok(format!("MCP server 已注册：{name}"))
    }

    pub(in crate::app_state) fn remove_mcp_server(
        self,
        state: &mut TiangongState,
        name: &str,
    ) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }

        let before = state.store.agent.agent_config.mcp.servers.len();
        state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .retain(|server| server.name != name);
        if state.store.agent.agent_config.mcp.servers.len() == before {
            return Err(anyhow!("未找到 MCP server：{name}"));
        }

        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        audit::append_audit_log(&audit::AuditEntry::new(
            "mcp.remove",
            name,
            &format!("MCP server 已删除：{name}"),
            true,
        ));
        Ok(format!("MCP server 已删除：{name}"))
    }

    pub(in crate::app_state) fn set_mcp_server_enabled(
        self,
        state: &mut TiangongState,
        name: &str,
        enabled: bool,
    ) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }

        let Some(server) = state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .iter_mut()
            .find(|server| server.name == name)
        else {
            return Err(anyhow!("未找到 MCP server：{name}"));
        };
        server.enabled = enabled;

        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        audit::append_audit_log(&audit::AuditEntry::new(
            "mcp.toggle",
            name,
            &format!("enabled={enabled}"),
            true,
        ));
        Ok(format!("MCP server 状态已更新：{name} enabled={enabled}"))
    }

    pub(in crate::app_state) fn update_agent_config_entry(
        self,
        state: &mut TiangongState,
        key: &str,
        value: &str,
    ) -> Result<String> {
        let key = key.trim();
        let value = value.trim();

        if key.is_empty() {
            return Err(anyhow!("配置键不能为空"));
        }

        let updated_value = match key {
            "skills.enabled" => {
                let parsed = parse_bool(value)?;
                state.store.agent.agent_config.skills.enabled = parsed;
                parsed.to_string()
            }
            "skills.max_matches" => {
                let parsed = value
                    .parse::<usize>()
                    .with_context(|| format!("配置值无效，要求正整数：{value}"))?;
                if parsed == 0 {
                    return Err(anyhow!("skills.max_matches 必须大于 0"));
                }
                state.store.agent.agent_config.skills.max_matches = parsed;
                parsed.to_string()
            }
            "skills.dirs" => {
                let parsed = parse_list_value(value);
                state.store.agent.agent_config.skills.dirs = parsed.clone();
                if parsed.is_empty() {
                    "(empty)".to_string()
                } else {
                    parsed.join(",")
                }
            }
            "mcp.enabled" => {
                let parsed = parse_bool(value)?;
                state.store.agent.agent_config.mcp.enabled = parsed;
                parsed.to_string()
            }
            "mcp.timeout_ms" => {
                let parsed = value
                    .parse::<u64>()
                    .with_context(|| format!("配置值无效，要求正整数：{value}"))?;
                if parsed == 0 {
                    return Err(anyhow!("mcp.timeout_ms 必须大于 0"));
                }
                state.store.agent.agent_config.mcp.timeout_ms = parsed;
                parsed.to_string()
            }
            _ => {
                return Err(anyhow!(
                    "不支持的配置键：{key}。支持：skills.enabled、skills.max_matches、skills.dirs、mcp.enabled、mcp.timeout_ms"
                ));
            }
        };

        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;

        Ok(format!("配置已更新：{key}={updated_value}"))
    }
}
