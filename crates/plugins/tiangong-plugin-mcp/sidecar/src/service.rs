//! MCP sidecar 业务服务：承载配置、capability 探测与工具执行，按操作名分发请求。
//!
//! 整合原 plugin.rs 的状态管理与 management.rs 的管理 CRUD，全部经 IPC 操作
//! 暴露给运行时（host 侧 invoke_sidecar）与 WASM 桥接。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use anyhow::{Context, Result, anyhow, bail};

use tiangong_plugin_mcp_protocol::config::{
    McpConfig, McpServerConfig, RegisterMcpServerRequest, UpdateConfigEntryRequest,
};
use tiangong_plugin_mcp_protocol::management::{
    CONFIG_GET_OPERATION, CONFIG_SNAPSHOT_OPERATION, CONFIG_UPDATE_ENTRY_OPERATION,
    RemoveServerRequest, SERVER_MERGE_DISK_OPERATION, SERVER_REGISTER_OPERATION,
    SERVER_REMOVE_OPERATION, SERVER_SET_ENABLED_OPERATION, SERVER_UPDATE_OPERATION,
    ServersResponse, SetEnabledRequest, UpdateServerRequest,
};
use tiangong_plugin_mcp_protocol::tool::{ExecuteToolResponse, ListToolsResponse};
use tiangong_plugin_mcp_protocol::{
    MCP_PROTOCOL_VERSION, MessageResponse, PLUGIN_ID, PLUGIN_VERSION,
};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};

use crate::capability::McpCapabilityIndex;
use crate::execution::{McpFunctionTarget, build_targets, execute_tool, list_tools_response};
use crate::paths::{default_mcp_capability_cache_path, default_mcp_config_path};
use crate::validate::{describe_mcp_servers, summarize_mcp_servers, validate_mcp_config};

/// MCP capability 后台刷新间隔（秒）。
const MCP_CAPABILITY_REFRESH_INTERVAL_SECS: u64 = 300;

/// MCP sidecar 业务服务。
pub struct McpService {
    mcp_config: RwLock<McpConfig>,
    mcp_targets: RwLock<std::collections::HashMap<String, McpFunctionTarget>>,
    capability: McpCapabilityIndex,
    capability_cache_path: PathBuf,
    mcp_config_path: PathBuf,
    /// 当前会话工作目录（由 reconfigure 注入，stdio MCP 子进程用）。
    workspace: RwLock<Option<PathBuf>>,
}

impl McpService {
    /// 用默认存储路径构造。
    pub fn new() -> Result<Self> {
        Self::with_paths(
            default_mcp_config_path(),
            default_mcp_capability_cache_path(),
        )
    }

    pub fn with_paths(mcp_config_path: PathBuf, capability_cache_path: PathBuf) -> Result<Self> {
        let mcp_config = load_mcp_config_from_path(&mcp_config_path);
        let capability = McpCapabilityIndex::new();
        let _ = capability.load_cache(&capability_cache_path);
        Ok(Self {
            mcp_config: RwLock::new(mcp_config),
            mcp_targets: RwLock::new(std::collections::HashMap::new()),
            capability,
            capability_cache_path,
            mcp_config_path,
            workspace: RwLock::new(None),
        })
    }

    fn config_snapshot(&self) -> McpConfig {
        self.mcp_config
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    #[allow(dead_code)]
    fn targets_snapshot(&self) -> std::collections::HashMap<String, McpFunctionTarget> {
        self.mcp_targets
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn reconfigure(&self) {
        let config = self.config_snapshot();
        self.capability.configure_scheduler(
            config.clone(),
            self.capability_cache_path.clone(),
            MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        self.capability.refresh_async(config.clone());
        self.rebuild_targets(&config);
    }

    fn sync_probe_and_rebuild(&self, server_name: &str) {
        let config = self.config_snapshot();
        if let Some(server) = config.servers.iter().find(|s| s.name == server_name)
            && server.enabled
        {
            let outcome = self.capability.probe_single(server, config.timeout_ms);
            if outcome.healthy {
                tracing::info!(
                    "MCP server 同步探测成功：server={} tools={}",
                    server_name,
                    outcome.tool_count
                );
            } else if let Some(err) = outcome.last_error {
                tracing::warn!(
                    "MCP server 同步探测失败（后台调度器会重试）：server={} error={}",
                    server_name,
                    err
                );
            }
        }
        self.rebuild_targets(&config);
    }

    fn rebuild_targets(&self, config: &McpConfig) {
        let active = self.capability.cached_active_tools();
        let targets = build_targets(config, active);
        if let Ok(mut guard) = self.mcp_targets.write() {
            *guard = targets;
        }
    }

    /// 按 operation 分发运行时请求，返回通用 Response。
    pub async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "MCP 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                    request.protocol_version
                ),
                false,
            );
        }
        match self
            .dispatch_operation(&request.operation, request.payload)
            .await
        {
            Ok(payload) => Response::success(&request_id, payload),
            Err(error) => Response::error(
                &request_id,
                ErrorCode::ServiceError,
                error.to_string(),
                false,
            ),
        }
    }

    async fn dispatch_operation(
        &self,
        operation: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        match operation {
            HANDSHAKE_OPERATION => serde_json::to_value(HandshakeResponse {
                plugin_id: PLUGIN_ID.to_string(),
                plugin_version: PLUGIN_VERSION.to_string(),
                sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol_version: PROTOCOL_VERSION.to_string(),
                business_protocol: MCP_PROTOCOL_VERSION,
                capabilities: vec!["mcp".to_string()],
                instance_id: format!("mcp-sidecar-{}", std::process::id()),
                status: ServiceStatus::Ready,
            })
            .with_context(|| "序列化 MCP 握手响应失败"),
            CONFIG_GET_OPERATION | CONFIG_SNAPSHOT_OPERATION => {
                serde_json::to_value(self.config_snapshot()).with_context(|| "序列化 MCP 配置失败")
            }
            CONFIG_UPDATE_ENTRY_OPERATION => {
                let req: UpdateConfigEntryRequest = serde_json::from_value(payload)?;
                let message = self.update_config_entry(&req.key, &req.value)?;
                Ok(serde_json::to_value(MessageResponse { message })?)
            }
            SERVER_REGISTER_OPERATION => {
                let req: RegisterMcpServerRequest = serde_json::from_value(payload)?;
                let message = self.register_server(req)?;
                Ok(serde_json::to_value(MessageResponse { message })?)
            }
            SERVER_UPDATE_OPERATION => {
                let req: UpdateServerRequest = serde_json::from_value(payload)?;
                let message = self.update_server(&req.name, req.request)?;
                Ok(serde_json::to_value(MessageResponse { message })?)
            }
            SERVER_REMOVE_OPERATION => {
                let req: RemoveServerRequest = serde_json::from_value(payload)?;
                let message = self.remove_server(&req.name)?;
                Ok(serde_json::to_value(MessageResponse { message })?)
            }
            SERVER_SET_ENABLED_OPERATION => {
                let req: SetEnabledRequest = serde_json::from_value(payload)?;
                let message = self.set_enabled(&req.name, req.enabled)?;
                Ok(serde_json::to_value(MessageResponse { message })?)
            }
            SERVER_MERGE_DISK_OPERATION => {
                self.merge_with_disk()?;
                Ok(serde_json::to_value(MessageResponse {
                    message: "已合并磁盘外部新增的 server".to_string(),
                })?)
            }
            tiangong_plugin_mcp_protocol::query::SERVER_LIST_OPERATION => {
                let servers = self.config_snapshot().servers;
                Ok(serde_json::to_value(ServersResponse { servers })?)
            }
            tiangong_plugin_mcp_protocol::query::SERVER_CACHED_TOOLS_OPERATION => {
                use tiangong_plugin_mcp_protocol::query::{ServerNameRequest, ToolsResponse};
                let req: ServerNameRequest = serde_json::from_value(payload)?;
                let tools = self
                    .capability
                    .cached_server_tools(&req.name)
                    .unwrap_or_default();
                Ok(serde_json::to_value(ToolsResponse { tools })?)
            }
            tiangong_plugin_mcp_protocol::query::SERVER_HEALTH_OPERATION => {
                use tiangong_plugin_mcp_protocol::query::HealthResponse;
                let statuses = self.capability.health_statuses();
                Ok(serde_json::to_value(HealthResponse { statuses })?)
            }
            tiangong_plugin_mcp_protocol::query::SERVER_SUMMARY_OPERATION => {
                use tiangong_plugin_mcp_protocol::NameFilterRequest;
                use tiangong_plugin_mcp_protocol::query::TextResponse;
                let req: NameFilterRequest = serde_json::from_value(payload)?;
                let text =
                    summarize_mcp_servers(&self.config_snapshot().servers, req.filter.as_deref());
                Ok(serde_json::to_value(TextResponse { text })?)
            }
            tiangong_plugin_mcp_protocol::query::SERVER_DETAIL_OPERATION => {
                use tiangong_plugin_mcp_protocol::NameFilterRequest;
                use tiangong_plugin_mcp_protocol::query::TextResponse;
                let req: NameFilterRequest = serde_json::from_value(payload)?;
                let text =
                    describe_mcp_servers(&self.config_snapshot().servers, req.filter.as_deref());
                Ok(serde_json::to_value(TextResponse { text })?)
            }
            tiangong_plugin_mcp_protocol::capability::SERVER_PROBE_OPERATION => {
                use tiangong_plugin_mcp_protocol::query::ServerNameRequest;
                let req: ServerNameRequest = serde_json::from_value(payload)?;
                self.probe_server(&req.name)?;
                Ok(serde_json::to_value(
                    tiangong_plugin_mcp_protocol::Empty {},
                )?)
            }
            tiangong_plugin_mcp_protocol::capability::RECONFIGURE_OPERATION => {
                let req: tiangong_plugin_mcp_protocol::capability::ReconfigureRequest =
                    serde_json::from_value(payload)?;
                if let Some(ws) = req.workspace
                    && let Ok(mut guard) = self.workspace.write()
                {
                    *guard = Some(PathBuf::from(ws));
                }
                self.reconfigure();
                Ok(serde_json::to_value(
                    tiangong_plugin_mcp_protocol::Empty {},
                )?)
            }
            tiangong_plugin_mcp_protocol::tool::LIST_TOOLS_OPERATION => {
                let config = self.config_snapshot();
                let active = self.capability.cached_active_tools();
                let response: ListToolsResponse = list_tools_response(&config, active);
                Ok(serde_json::to_value(response)?)
            }
            tiangong_plugin_mcp_protocol::tool::EXECUTE_TOOL_OPERATION => {
                use tiangong_plugin_mcp_protocol::tool::ExecuteToolRequest;
                let req: ExecuteToolRequest = serde_json::from_value(payload)?;
                let config = self.config_snapshot();
                let target = McpFunctionTarget {
                    server_name: req.server_name,
                    tool_name: req.tool_name,
                };
                let workspace = req
                    .workspace
                    .clone()
                    .map(PathBuf::from)
                    .or_else(|| self.workspace.read().ok().and_then(|guard| guard.clone()));
                let result: ExecuteToolResponse =
                    execute_tool(&target, req.arguments, &config, workspace)
                        .await
                        .unwrap_or_else(|err| ExecuteToolResponse {
                            ok: false,
                            summary: format!("MCP工具调用失败：{err}"),
                            stderr: err.to_string(),
                            exit_code: 1,
                            ..Default::default()
                        });
                Ok(serde_json::to_value(result)?)
            }
            tiangong_plugin_mcp_protocol::env::ENV_COLLECT_OPERATION => {
                let env: BTreeMap<String, String> = self.collect_runtime_env();
                Ok(serde_json::to_value(env)?)
            }
            other => bail!("不支持的 MCP 操作: {other}"),
        }
    }

    fn apply_config(&self, next: McpConfig, affected_server: Option<&str>) -> Result<()> {
        write_mcp_config_to_path(&self.mcp_config_path, &next)?;
        if let Ok(mut guard) = self.mcp_config.write() {
            *guard = next.clone();
        } else {
            return Err(anyhow!("MCP 配置锁中毒"));
        }
        self.capability.configure_scheduler(
            next.clone(),
            self.capability_cache_path.clone(),
            MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        match affected_server {
            Some(name) => self.sync_probe_and_rebuild(name),
            None => self.rebuild_targets(&next),
        }
        Ok(())
    }

    fn register_server(&self, request: RegisterMcpServerRequest) -> Result<String> {
        let name = request.name.trim().to_string();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }
        let fields = normalize_request_fields(request)?;
        if fields.command.is_empty() && fields.endpoint.is_empty() && fields.tags.is_empty() {
            return Err(anyhow!("MCP server 需至少配置 command、endpoint 或 tags"));
        }
        let mut next = self.config_snapshot();
        if next.servers.iter().any(|s| s.name == name) {
            return Err(anyhow!("MCP server 已存在：{name}"));
        }
        next.servers.push(McpServerConfig {
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
        validate_mcp_config(&next)?;
        self.apply_config(next, Some(&name))?;
        Ok(format!("MCP server 已注册：{name}"))
    }

    fn update_server(&self, name: &str, request: RegisterMcpServerRequest) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }
        let fields = normalize_request_fields(request)?;
        if fields.command.is_empty() && fields.endpoint.is_empty() && fields.tags.is_empty() {
            return Err(anyhow!("MCP server 需至少配置 command、endpoint 或 tags"));
        }
        let mut next = self.config_snapshot();
        let server = next
            .servers
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow!("未找到 MCP server：{name}"))?;
        server.transport = fields.transport;
        server.command = fields.command;
        server.args = fields.args;
        server.endpoint = fields.endpoint;
        server.auth_header = fields.auth_header;
        server.headers = fields.headers;
        server.env = fields.env;
        server.tags = fields.tags;
        validate_mcp_config(&next)?;
        self.apply_config(next, Some(name))?;
        Ok(format!("MCP server 已更新：{name}"))
    }

    fn remove_server(&self, name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }
        let mut next = self.config_snapshot();
        let before = next.servers.len();
        next.servers.retain(|s| s.name != name);
        if next.servers.len() == before {
            return Err(anyhow!("未找到 MCP server：{name}"));
        }
        validate_mcp_config(&next)?;
        self.apply_config(next, None)?;
        Ok(format!("MCP server 已删除：{name}"))
    }

    fn set_enabled(&self, name: &str, enabled: bool) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }
        let mut next = self.config_snapshot();
        let server = next
            .servers
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| anyhow!("未找到 MCP server：{name}"))?;
        server.enabled = enabled;
        validate_mcp_config(&next)?;
        self.apply_config(next, Some(name))?;
        Ok(format!("MCP server 状态已更新：{name} enabled={enabled}"))
    }

    fn update_config_entry(&self, key: &str, value: &str) -> Result<String> {
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            return Err(anyhow!("配置键不能为空"));
        }
        let mut next = self.config_snapshot();
        let updated = match key {
            "mcp.enabled" => {
                let parsed = parse_bool(value)?;
                next.enabled = parsed;
                parsed.to_string()
            }
            "mcp.timeout_ms" => {
                let parsed: u64 = value
                    .parse()
                    .with_context(|| format!("配置值无效，要求正整数：{value}"))?;
                if parsed == 0 {
                    return Err(anyhow!("mcp.timeout_ms 必须大于 0"));
                }
                next.timeout_ms = parsed;
                parsed.to_string()
            }
            _ => {
                return Err(anyhow!(
                    "不支持的配置键：{key}。支持：mcp.enabled、mcp.timeout_ms"
                ));
            }
        };
        validate_mcp_config(&next)?;
        self.apply_config(next, None)?;
        Ok(format!("配置已更新：{key}={updated}"))
    }

    fn probe_server(&self, name: &str) -> Result<()> {
        self.capability.probe_single_by_name(name)?;
        let config = self.config_snapshot();
        self.rebuild_targets(&config);
        Ok(())
    }

    fn merge_with_disk(&self) -> Result<()> {
        if !self.mcp_config_path.exists() {
            return Ok(());
        }
        let disk_content = std::fs::read_to_string(&self.mcp_config_path)
            .with_context(|| format!("读取 mcp 配置失败：{}", self.mcp_config_path.display()))?;
        let disk_mcp: McpConfig = serde_json::from_str(&disk_content)
            .with_context(|| format!("解析 mcp 配置失败：{}", self.mcp_config_path.display()))?;
        let mut next = self.config_snapshot();
        let memory_names: Vec<String> = next.servers.iter().map(|s| s.name.clone()).collect();
        let to_add: Vec<McpServerConfig> = disk_mcp
            .servers
            .into_iter()
            .filter(|s| !memory_names.contains(&s.name))
            .collect();
        if to_add.is_empty() {
            return Ok(());
        }
        next.servers.extend(to_add);
        validate_mcp_config(&next)?;
        self.apply_config(next, None)
    }

    /// 收集启用 MCP server 的环境变量（exec_env 回传）。
    pub fn collect_runtime_env(&self) -> BTreeMap<String, String> {
        let mut runtime_env = BTreeMap::new();
        let config = self.config_snapshot();
        if config.enabled {
            for server in &config.servers {
                if !server.enabled {
                    continue;
                }
                for (key, value) in &server.env {
                    let key = key.trim();
                    if !key.is_empty() {
                        runtime_env.insert(key.to_string(), value.trim().to_string());
                    }
                }
            }
        }
        runtime_env
    }
}

/// 从指定路径加载 MCP 配置；文件不存在或解析失败时返回默认配置。
fn load_mcp_config_from_path(path: &std::path::Path) -> McpConfig {
    if !path.exists() {
        return McpConfig::default();
    }
    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<McpConfig>(&content) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(
                    "MCP 配置解析失败，回退为默认配置：path={} error={err}",
                    path.display()
                );
                McpConfig::default()
            }
        },
        Err(err) => {
            tracing::warn!(
                "MCP 配置读取失败，回退为默认配置：path={} error={err}",
                path.display()
            );
            McpConfig::default()
        }
    }
}

fn write_mcp_config_to_path(path: &std::path::Path, config: &McpConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 MCP 配置目录失败：{}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(config).context("序列化 mcp 配置失败")?;
    std::fs::write(path, content)
        .with_context(|| format!("写入 mcp 配置失败：{}", path.display()))?;
    Ok(())
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(anyhow!("布尔值无效：{raw}（可用 true/false）")),
    }
}

/// 规范化注册/编辑请求字段。
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

struct NormalizedMcpFields {
    transport: tiangong_plugin_mcp_protocol::config::McpTransportMode,
    command: String,
    args: Vec<String>,
    endpoint: String,
    auth_header: String,
    headers: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    enabled: bool,
    tags: Vec<String>,
}

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for McpService {
    async fn dispatch(
        &self,
        request: tiangong_plugin_runtime::protocol::Request,
    ) -> tiangong_plugin_runtime::protocol::Response {
        McpService::dispatch(self, request).await
    }
}
