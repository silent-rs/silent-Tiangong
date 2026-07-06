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
        let name = request.name.trim().to_string();
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

        let fields = normalize_request_fields(request)?;
        if fields.command.is_empty() && fields.endpoint.is_empty() && fields.tags.is_empty() {
            return Err(anyhow!("MCP server 需至少配置 command、endpoint 或 tags"));
        }

        state
            .store
            .agent
            .agent_config
            .mcp
            .servers
            .push(McpServerConfig {
                name: name.clone(),
                transport: fields.transport,
                command: fields.command,
                args: fields.args,
                endpoint: fields.endpoint,
                auth_header: fields.auth_header,
                headers: fields.headers,
                env: fields.env,
                enabled: fields.enabled,
                tags: fields.tags,
            });
        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        state.persist_agent_configs_only()?;
        audit::append_audit_log(&audit::AuditEntry::new(
            "mcp.register",
            &name,
            &format!("MCP server 已注册：{name}"),
            true,
        ));
        Ok(format!("MCP server 已注册：{name}"))
    }

    pub(in crate::app_state) fn update_mcp_server(
        self,
        state: &mut TiangongState,
        name: &str,
        request: RegisterMcpServerRequest,
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

        let fields = normalize_request_fields(request)?;
        if fields.command.is_empty() && fields.endpoint.is_empty() && fields.tags.is_empty() {
            return Err(anyhow!("MCP server 需至少配置 command、endpoint 或 tags"));
        }

        // name 作为主键保持不变，就地更新其余字段。
        // enabled 由列表里的开关单独控制，编辑表单不覆盖它。
        server.transport = fields.transport;
        server.command = fields.command;
        server.args = fields.args;
        server.endpoint = fields.endpoint;
        server.auth_header = fields.auth_header;
        server.headers = fields.headers;
        server.env = fields.env;
        server.tags = fields.tags;

        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        state.persist_agent_configs_only()?;
        audit::append_audit_log(&audit::AuditEntry::new(
            "mcp.update",
            name,
            &format!("MCP server 已更新：{name}"),
            true,
        ));
        Ok(format!("MCP server 已更新：{name}"))
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
        // 跳过 MCP 磁盘合并：否则 merge_mcp_with_disk 会将刚删除的 server
        // 视为"其他进程新增"而重新加回，导致删除无法持久化。
        state.persist_agent_configs_no_merge_mcp()?;
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
        state.persist_agent_configs_only()?;
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
                    "不支持的配置键：{key}。支持：mcp.enabled、mcp.timeout_ms（skills 配置已迁移至 skill plugin）"
                ));
            }
        };

        validate_agent_config(&state.store.agent.agent_config)?;
        state.rebuild_runtime_for_agent_config();
        state.persist_app_only()?;
        state.persist_agent_configs_only()?;

        Ok(format!("配置已更新：{key}={updated_value}"))
    }
}

/// 规范化后的 MCP server 字段（不含 name，name 作为主键由调用方处理）。
struct NormalizedMcpFields {
    transport: McpTransportMode,
    command: String,
    args: Vec<String>,
    endpoint: String,
    auth_header: String,
    headers: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    enabled: bool,
    tags: Vec<String>,
}

/// 把注册/编辑请求规范化为可直接写入 McpServerConfig 的字段。
/// register_mcp_server 与 update_mcp_server 共用，避免 trim/filter 逻辑重复。
fn normalize_request_fields(request: RegisterMcpServerRequest) -> Result<NormalizedMcpFields> {
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
        .collect::<BTreeMap<_, _>>();
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
        .collect::<BTreeMap<_, _>>();

    Ok(NormalizedMcpFields {
        transport,
        command: request.command.trim().to_string(),
        args,
        endpoint,
        auth_header,
        headers,
        env,
        enabled: request.enabled,
        tags,
    })
}
