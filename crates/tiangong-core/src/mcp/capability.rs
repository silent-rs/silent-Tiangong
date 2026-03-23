use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent_config::{McpConfig, McpServerConfig};

use super::client::{LocalMcpClient, McpClient, McpToolMeta};

const MIN_REFRESH_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct McpServerCapability {
    #[serde(default, deserialize_with = "deserialize_tools_compat")]
    tools: Vec<McpToolMeta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedCapabilityCache {
    #[serde(default)]
    servers: BTreeMap<String, McpServerCapability>,
}

#[derive(Debug, Clone, Default)]
struct CapabilitySchedulerConfig {
    mcp_config: McpConfig,
    cache_path: Option<PathBuf>,
    refresh_interval_secs: u64,
}

static MCP_CAPABILITY_INDEX: OnceLock<RwLock<BTreeMap<String, McpServerCapability>>> =
    OnceLock::new();
static MCP_CAPABILITY_SCHEDULER: OnceLock<RwLock<CapabilitySchedulerConfig>> = OnceLock::new();
static MCP_CAPABILITY_SCHEDULER_STARTED: OnceLock<()> = OnceLock::new();

fn capability_index() -> &'static RwLock<BTreeMap<String, McpServerCapability>> {
    MCP_CAPABILITY_INDEX.get_or_init(|| RwLock::new(BTreeMap::new()))
}

fn capability_scheduler() -> &'static RwLock<CapabilitySchedulerConfig> {
    MCP_CAPABILITY_SCHEDULER.get_or_init(|| RwLock::new(CapabilitySchedulerConfig::default()))
}

pub fn load_mcp_capabilities_cache(cache_path: &Path) -> Result<()> {
    if !cache_path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(cache_path)
        .with_context(|| format!("读取 MCP tools 缓存失败：{}", cache_path.display()))?;
    let payload: PersistedCapabilityCache =
        serde_json::from_str(&content).context("解析 MCP tools 缓存失败")?;
    if let Ok(mut guard) = capability_index().write() {
        *guard = payload.servers;
    }
    Ok(())
}

pub fn persist_mcp_capabilities_cache(cache_path: &Path) -> Result<()> {
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 MCP tools 缓存目录失败：{}", parent.display()))?;
    }
    let servers = capability_index()
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let payload = PersistedCapabilityCache { servers };
    let content = serde_json::to_string_pretty(&payload).context("序列化 MCP tools 缓存失败")?;
    fs::write(cache_path, content)
        .with_context(|| format!("写入 MCP tools 缓存失败：{}", cache_path.display()))?;
    Ok(())
}

pub fn configure_mcp_capability_scheduler(
    config: McpConfig,
    cache_path: PathBuf,
    refresh_interval_secs: u64,
) {
    if let Ok(mut guard) = capability_scheduler().write() {
        guard.mcp_config = config;
        guard.cache_path = Some(cache_path);
        guard.refresh_interval_secs = refresh_interval_secs.max(MIN_REFRESH_INTERVAL_SECS);
    }
    MCP_CAPABILITY_SCHEDULER_STARTED.get_or_init(|| {
        let _ = thread::Builder::new()
            .name("tiangong-mcp-capability-scheduler".to_string())
            .spawn(|| {
                loop {
                    let interval_secs = capability_scheduler()
                        .read()
                        .map(|guard| guard.refresh_interval_secs.max(MIN_REFRESH_INTERVAL_SECS))
                        .unwrap_or(MIN_REFRESH_INTERVAL_SECS);
                    thread::sleep(Duration::from_secs(interval_secs));
                    let (config, cache_path) = capability_scheduler()
                        .read()
                        .map(|guard| (guard.mcp_config.clone(), guard.cache_path.clone()))
                        .unwrap_or_else(|_| (McpConfig::default(), None));
                    if !config.enabled || config.servers.is_empty() {
                        continue;
                    }
                    refresh_mcp_capabilities(&config);
                    if let Some(path) = cache_path.as_deref() {
                        let _ = persist_mcp_capabilities_cache(path);
                    }
                }
            });
    });
}

pub fn refresh_mcp_capabilities_async(config: McpConfig) {
    if let Ok(mut guard) = capability_scheduler().write() {
        guard.mcp_config = config.clone();
    }
    let _ = thread::Builder::new()
        .name("tiangong-mcp-capability-prewarm".to_string())
        .spawn(move || {
            refresh_mcp_capabilities(&config);
            if let Some(path) = capability_scheduler()
                .read()
                .ok()
                .and_then(|guard| guard.cache_path.clone())
            {
                let _ = persist_mcp_capabilities_cache(&path);
            }
        });
}

pub fn refresh_mcp_capabilities(config: &McpConfig) {
    let mut map = BTreeMap::new();
    for server in config.servers.iter().filter(|server| server.enabled) {
        let capability = probe_server_capability(server, config.timeout_ms);
        map.insert(server.name.clone(), capability);
    }
    if let Ok(mut guard) = capability_index().write() {
        *guard = map;
    }
}

pub fn cached_server_tools(server_name: &str) -> Option<Vec<McpToolMeta>> {
    let guard = capability_index().read().ok()?;
    guard.get(server_name).map(|entry| entry.tools.clone())
}

pub fn cached_active_tools() -> Vec<(String, Vec<McpToolMeta>)> {
    capability_index()
        .read()
        .map(|guard| {
            guard
                .iter()
                .map(|(name, capability)| (name.clone(), capability.tools.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn build_mcp_tools_system_prompt(max_tools_per_server: usize) -> Option<String> {
    let servers = cached_active_tools();
    if servers.is_empty() {
        return None;
    }
    let per_server_limit = max_tools_per_server.max(1);
    let mut lines = Vec::new();
    lines.push("可用 MCP 工具（已启用 server，按缓存注入）：".to_string());
    for (server_name, tools) in servers {
        lines.push(format!("server={} tool_count={}", server_name, tools.len()));
        for tool in tools.iter().take(per_server_limit) {
            lines.push(format!(
                "- {} | desc={} | args={}",
                tool.compact_signature(),
                tool.description.trim(),
                tool.argument_brief(8)
            ));
        }
        if tools.len() > per_server_limit {
            lines.push(format!(
                "- ... 省略 {} 个工具",
                tools.len() - per_server_limit
            ));
        }
    }
    Some(lines.join("\n"))
}

fn probe_server_capability(server: &McpServerConfig, timeout_ms: u64) -> McpServerCapability {
    let client = LocalMcpClient;
    let raw_tools = client.list_tools(server, timeout_ms).unwrap_or_default();
    let tools = dedup_tools(&raw_tools);
    McpServerCapability { tools }
}

fn dedup_tools(items: &[McpToolMeta]) -> Vec<McpToolMeta> {
    let mut output = BTreeMap::new();
    for item in items {
        let name = item.name.trim();
        if !name.is_empty() {
            output
                .entry(name.to_string())
                .or_insert_with(|| item.clone());
        }
    }
    output.into_values().collect()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum LegacyToolCompat {
    Name(String),
    Tool(McpToolMeta),
}

fn deserialize_tools_compat<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<McpToolMeta>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<LegacyToolCompat>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|item| match item {
            LegacyToolCompat::Name(name) => {
                let name = name.trim();
                if name.is_empty() {
                    None
                } else {
                    Some(McpToolMeta {
                        name: name.to_string(),
                        description: String::new(),
                        input_schema: serde_json::Value::Null,
                        argument_summaries: Vec::new(),
                    })
                }
            }
            LegacyToolCompat::Tool(tool) => {
                if tool.name.trim().is_empty() {
                    None
                } else {
                    Some(tool)
                }
            }
        })
        .collect())
}
