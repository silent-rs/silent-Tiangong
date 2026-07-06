//! MCP 管理 API——作为 [`McpPlugin`] 的固有方法。
//!
//! 对齐 [`tiangong_plugin_skill::SkillPlugin`] 的 management 模式：管理逻辑
//! 直接实现在插件上，操作自托管配置（`~/.tiangong/mcp.json`），不依赖
//! `TiangongState`。原属 `tiangong_core::app_state::services::mcp::AppMcpService`。

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow};

use tiangong_core::app_state::audit::{AuditEntry, append_audit_log};

use crate::capability::McpServerHealthStatus;
use crate::client::McpToolMeta;
use crate::config::{
    McpConfig, McpServerConfig, RegisterMcpServerRequest, normalize_request_fields,
};
use crate::plugin::McpPlugin;
use crate::validate::{describe_mcp_servers, summarize_mcp_servers, validate_mcp_config};

impl McpPlugin {
    /// 返回自托管 MCP servers 的快照。
    pub fn mcp_servers(&self) -> Vec<McpServerConfig> {
        self.config_snapshot().servers
    }

    /// MCP server 缓存的工具列表（供前端展示）。
    pub fn mcp_server_cached_tools(&self, name: &str) -> Option<Vec<McpToolMeta>> {
        self.capability.cached_server_tools(name)
    }

    /// 所有 MCP server 的健康状态（供前端健康面板）。
    pub fn mcp_server_health_statuses(&self) -> Vec<McpServerHealthStatus> {
        self.capability.health_statuses()
    }

    /// 文本摘要（供 CLI `mcp list`）。
    pub fn mcp_server_summary(&self, name_filter: Option<&str>) -> String {
        let servers = self.mcp_servers();
        summarize_mcp_servers(&servers, name_filter)
    }

    /// 文本详情（供 CLI `mcp show`）。
    pub fn mcp_server_detail(&self, name_filter: Option<&str>) -> String {
        let servers = self.mcp_servers();
        describe_mcp_servers(&servers, name_filter)
    }

    /// 探测单个 MCP server 并刷新缓存（供前端"重试"按钮）。
    ///
    /// 探测成功后重建 mcp_targets 绑定，确保 retry 成功的工具立即对 LLM 可见。
    pub fn probe_mcp_server(&self, name: &str) -> Result<()> {
        self.capability.probe_single_by_name(name)?;
        // 探测可能改变 capability cache（失败→成功），重建 targets 让新工具生效。
        let config = self.config_snapshot();
        self.rebuild_targets(&config);
        Ok(())
    }

    /// 注册新的 MCP server。
    pub fn register_mcp_server(&self, request: RegisterMcpServerRequest) -> Result<String> {
        let name = request.name.trim().to_string();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }

        let fields = normalize_request_fields(request)?;
        if fields.command.is_empty() && fields.endpoint.is_empty() && fields.tags.is_empty() {
            return Err(anyhow!("MCP server 需至少配置 command、endpoint 或 tags"));
        }

        // copy-on-write：在 snapshot 上修改 + validate，成功后整体写回，
        // 校验失败不污染内存状态。
        let mut next = self.config_snapshot();
        if next.servers.iter().any(|server| server.name == name) {
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
        self.commit_config(next)?;

        self.persist_and_sync_probe(Some(&name))?;
        append_audit_log(&AuditEntry::new(
            "mcp.register",
            &name,
            &format!("MCP server 已注册：{name}"),
            true,
        ));
        Ok(format!("MCP server 已注册：{name}"))
    }

    /// 更新已有 MCP server（name 作为主键保持不变，就地更新其余字段）。
    pub fn update_mcp_server(
        &self,
        name: &str,
        request: RegisterMcpServerRequest,
    ) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }

        let fields = normalize_request_fields(request)?;
        if fields.command.is_empty() && fields.endpoint.is_empty() && fields.tags.is_empty() {
            return Err(anyhow!("MCP server 需至少配置 command、endpoint 或 tags"));
        }

        // copy-on-write：在 snapshot 上修改 + validate，成功后整体写回。
        let mut next = self.config_snapshot();
        let server = next
            .servers
            .iter_mut()
            .find(|server| server.name == name)
            .ok_or_else(|| anyhow!("未找到 MCP server：{name}"))?;
        // enabled 由列表里的开关单独控制，编辑表单不覆盖它。
        server.transport = fields.transport;
        server.command = fields.command;
        server.args = fields.args;
        server.endpoint = fields.endpoint;
        server.auth_header = fields.auth_header;
        server.headers = fields.headers;
        server.env = fields.env;
        server.tags = fields.tags;
        validate_mcp_config(&next)?;
        self.commit_config(next)?;

        self.persist_and_sync_probe(Some(name))?;
        append_audit_log(&AuditEntry::new(
            "mcp.update",
            name,
            &format!("MCP server 已更新：{name}"),
            true,
        ));
        Ok(format!("MCP server 已更新：{name}"))
    }

    /// 删除 MCP server（不合并磁盘外部新增，确保删除持久化）。
    pub fn remove_mcp_server(&self, name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }

        // copy-on-write：在 snapshot 上删除 + validate，成功后整体写回。
        let mut next = self.config_snapshot();
        let before = next.servers.len();
        next.servers.retain(|server| server.name != name);
        if next.servers.len() == before {
            return Err(anyhow!("未找到 MCP server：{name}"));
        }
        validate_mcp_config(&next)?;
        self.commit_config(next)?;

        self.persist_and_sync_probe(None)?;
        append_audit_log(&AuditEntry::new(
            "mcp.remove",
            name,
            &format!("MCP server 已删除：{name}"),
            true,
        ));
        Ok(format!("MCP server 已删除：{name}"))
    }

    /// 启用/禁用 MCP server。
    pub fn set_mcp_server_enabled(&self, name: &str, enabled: bool) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("MCP server 名称不能为空"));
        }

        // copy-on-write：在 snapshot 上修改 + validate，成功后整体写回。
        let mut next = self.config_snapshot();
        let server = next
            .servers
            .iter_mut()
            .find(|server| server.name == name)
            .ok_or_else(|| anyhow!("未找到 MCP server：{name}"))?;
        server.enabled = enabled;
        validate_mcp_config(&next)?;
        self.commit_config(next)?;

        self.persist_and_sync_probe(Some(name))?;
        append_audit_log(&AuditEntry::new(
            "mcp.toggle",
            name,
            &format!("enabled={enabled}"),
            true,
        ));
        Ok(format!("MCP server 状态已更新：{name} enabled={enabled}"))
    }

    /// 更新顶层 MCP 配置项（enabled / timeout_ms）。
    pub fn update_mcp_config_entry(&self, key: &str, value: &str) -> Result<String> {
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            return Err(anyhow!("配置键不能为空"));
        }

        // copy-on-write：在 snapshot 上修改 + validate，成功后整体写回。
        let mut next = self.config_snapshot();
        let updated_value = match key {
            "mcp.enabled" => {
                let parsed = parse_bool(value)?;
                next.enabled = parsed;
                parsed.to_string()
            }
            "mcp.timeout_ms" => {
                let parsed = value
                    .parse::<u64>()
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
        self.commit_config(next)?;

        self.persist_and_sync_probe(None)?;
        Ok(format!("配置已更新：{key}={updated_value}"))
    }

    /// 把校验通过的配置写回内存（copy-on-write 的提交步骤）。
    fn commit_config(&self, next: McpConfig) -> Result<()> {
        if let Ok(mut guard) = self.mcp_config.write() {
            *guard = next;
            Ok(())
        } else {
            Err(anyhow!("MCP 配置锁中毒"))
        }
    }

    /// 持久化配置 + 同步探测受影响 server + 重建 targets。
    ///
    /// `affected_server` 传 `Some(name)` 时，同步探测该 server（确保管理操作返回时
    /// 工具即用）；传 `None` 时（remove/disable）只重建 targets（无需探测已删/禁用的）。
    /// 同时重配后台 scheduler 以更新它持有的 config 快照。
    fn persist_and_sync_probe(&self, affected_server: Option<&str>) -> Result<()> {
        let config = self.config_snapshot();
        crate::plugin::write_mcp_config_to_path(self.mcp_config_path(), &config)?;
        // 更新后台 scheduler 持有的 config 快照（它仍会周期性全量探测兜底）。
        self.capability.configure_scheduler(
            config.clone(),
            self.capability_cache_path.clone(),
            crate::plugin::MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        match affected_server {
            Some(name) => self.sync_probe_and_rebuild(name),
            None => self.rebuild_targets(&config),
        }
        Ok(())
    }

    /// 合并磁盘上其他进程新增的 MCP server（多进程共存）。
    ///
    /// 同名 server 以内存（当前进程）为准，磁盘上独有的 server 保留。
    pub fn merge_with_disk(&self) -> Result<()> {
        let path = self.mcp_config_path();
        if !path.exists() {
            return Ok(());
        }
        let disk_content = std::fs::read_to_string(path)
            .with_context(|| format!("读取 mcp 配置失败：{}", path.display()))?;
        let disk_mcp: McpConfig = serde_json::from_str(&disk_content)
            .with_context(|| format!("解析 mcp 配置失败：{}", path.display()))?;

        let mut external_added = Vec::new();
        {
            let mut config = self
                .mcp_config
                .write()
                .map_err(|_| anyhow!("MCP 配置锁中毒"))?;
            let memory_names: Vec<String> = config.servers.iter().map(|s| s.name.clone()).collect();
            let to_add: Vec<McpServerConfig> = disk_mcp
                .servers
                .into_iter()
                .filter(|s| !memory_names.contains(&s.name))
                .collect();
            for server in &to_add {
                external_added.push(server.name.clone());
            }
            config.servers.extend(to_add);
        }

        if !external_added.is_empty() {
            tracing::warn!(
                "检测到 mcp.json 被其他进程修改，已合并外部新增的 server：{}",
                external_added.join(", ")
            );
        }
        Ok(())
    }

    /// 收集启用的 MCP server 的环境变量（供子进程注入）。
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
                    if key.is_empty() {
                        continue;
                    }
                    runtime_env.insert(key.to_string(), value.trim().to_string());
                }
            }
        }
        runtime_env
    }
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(anyhow!("布尔值无效：{raw}（可用 true/false）")),
    }
}
