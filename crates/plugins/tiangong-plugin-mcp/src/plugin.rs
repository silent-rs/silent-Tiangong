//! MCP 管理插件——自治状态 + [`Plugin`] trait 实现。
//!
//! 对齐 [`tiangong_plugin_skill::SkillPlugin`] 的自治配方：plugin 自托管
//! [`McpConfig`]（读写 `~/.tiangong/mcp.json`）、`mcp_targets` 绑定与
//! [`McpCapabilityIndex`]（capability 缓存 + 后台调度器），core 不再持有 MCP 概念。
//!
//! 状态隔离：capability 缓存与调度器作为 plugin 实例字段（非全局 static），
//! 每个 plugin 实例独立一份，避免多实例（测试隔离 / server API 与 core 共存）
//! 场景下互相污染。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use tiangong_core::core::Plugin;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::permission::TrustMode;
use tiangong_core::runtime::RuntimeEngine;

use crate::capability::McpCapabilityIndex;
use crate::config::McpConfig;
use crate::execution::{McpFunctionTarget, execution_function_tools};
use crate::paths::{default_mcp_capability_cache_path, default_mcp_config_path};

/// MCP 管理插件。
///
/// 自托管 MCP 配置（`~/.tiangong/mcp.json`）与动态工具名→目标绑定，
/// 通过 [`Plugin`] 的 supertrait 自动向 core 暴露 MCP 工具规格与执行处理器。
/// 入口层（App/CLI/Tauri）持有 `Arc<McpPlugin>` 做管理（register/remove/...）。
pub struct McpPlugin {
    /// 自托管 MCP 配置（读写 `~/.tiangong/mcp.json`）。
    pub(crate) mcp_config: RwLock<McpConfig>,
    /// 动态工具名 → (server, tool) 目标绑定（engine build 时快照）。
    pub(crate) mcp_targets: RwLock<HashMap<String, McpFunctionTarget>>,
    /// MCP 能力索引（capability 缓存 + 后台刷新调度器），实例隔离。
    pub(crate) capability: McpCapabilityIndex,
    /// MCP tools 缓存路径（`~/.tiangong/mcp-tools-cache.json`）。
    pub(crate) capability_cache_path: PathBuf,
    /// MCP 配置文件路径（`~/.tiangong/mcp.json`）。
    pub(crate) mcp_config_path: PathBuf,
    /// 当前会话工作目录（由 core 注入）。
    pub(crate) workspace: RwLock<Option<PathBuf>>,
    /// 状态反馈通道（保持与其他插件一致的注入接口）。
    pub(crate) feedback_tx: RwLock<Option<PluginFeedbackTx>>,
}

impl McpPlugin {
    /// 用默认存储路径（`~/.tiangong/`）构造插件。
    pub fn new() -> Self {
        Self::with_paths(
            default_mcp_config_path(),
            default_mcp_capability_cache_path(),
        )
    }

    /// 用应用层注入的存储根目录（`~/.tiangong/`）构造插件。
    ///
    /// 由 entry 层在启动序列中调用，把统一解析的 storage root 注入进来，
    /// 避免 plugin 各自重复解析 `~/.tiangong`。无 app 上下文的场景（doctor /
    /// 孤立子命令）仍用 [`new`](Self::new) 走 plugin 自治回退。
    pub fn with_storage_root(root: PathBuf) -> Self {
        Self::with_paths(root.join("mcp.json"), root.join("mcp-tools-cache.json"))
    }

    /// 用显式路径构造（主要供测试使用，capability 状态实例隔离）。
    pub fn with_paths(mcp_config_path: PathBuf, capability_cache_path: PathBuf) -> Self {
        let mcp_config = load_mcp_config_from_path(&mcp_config_path);
        Self {
            mcp_config: RwLock::new(mcp_config),
            mcp_targets: RwLock::new(HashMap::new()),
            capability: McpCapabilityIndex::new(),
            capability_cache_path,
            mcp_config_path,
            workspace: RwLock::new(None),
            feedback_tx: RwLock::new(None),
        }
    }

    /// 读取自托管 MCP 配置的快照。
    pub(crate) fn config_snapshot(&self) -> McpConfig {
        self.mcp_config
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// 读取自托管 MCP 配置的快照（public，供入口层展示用）。
    pub fn config_snapshot_public(&self) -> McpConfig {
        self.config_snapshot()
    }

    /// MCP 配置文件路径。
    pub(crate) fn mcp_config_path(&self) -> &std::path::Path {
        &self.mcp_config_path
    }

    /// MCP tools 缓存路径。
    #[allow(dead_code)]
    pub(crate) fn capability_cache_path(&self) -> &std::path::Path {
        &self.capability_cache_path
    }

    /// 当前 mcp_targets 绑定快照（供 handler 分发用）。
    pub(crate) fn targets_snapshot(&self) -> HashMap<String, McpFunctionTarget> {
        self.mcp_targets
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// 当前所有 healthy server 的工具列表快照（供入口层 @mcp 补全展示工具数）。
    pub fn cached_active_tools(&self) -> Vec<(String, Vec<crate::client::McpToolMeta>)> {
        self.capability.cached_active_tools()
    }

    /// 重配 capability scheduler + 异步刷新 + 重建 mcp_targets 绑定。
    ///
    /// 在 register、engine rebuild 时调用。统一走异步刷新（不阻塞启动）：
    /// capability 探测在后台线程完成，完成后由后台 scheduler 周期性刷新；
    /// 首次启动若 cache 为空，MCP 工具会在下一次 engine rebuild（异步探测完成后
    /// 触发的 config 变更 / 手动操作）对 LLM 可见——这是当前架构下可接受的行为。
    pub(crate) fn reconfigure(&self) {
        let config = self.config_snapshot();
        self.capability.configure_scheduler(
            config.clone(),
            self.capability_cache_path.clone(),
            MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        self.capability.refresh_async(config.clone());
        self.rebuild_targets(&config);
    }

    /// 同步探测单个 server + 重建 mcp_targets 绑定。
    ///
    /// 供管理操作（register/update/set_enabled）使用：操作完成后同步探测受影响
    /// 的 server，探测完重建 targets，确保管理操作返回时新工具立即对 LLM 可见。
    pub(crate) fn sync_probe_and_rebuild(&self, server_name: &str) {
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
                    "MCP server 同步探测失败（工具暂不可用，后台调度器会重试）：server={} error={}",
                    server_name,
                    err
                );
            }
        }
        self.rebuild_targets(&config);
    }

    /// 根据当前 capability 缓存重建工具名→目标绑定。
    pub(crate) fn rebuild_targets(&self, config: &McpConfig) {
        let active = self.capability.cached_active_tools();
        let (specs, targets) =
            execution_function_tools(config, active, std::collections::HashSet::new());
        if let Ok(mut guard) = self.mcp_targets.write() {
            *guard = targets;
        }
        // specs 由 tool_specs() 独立收集，此处仅重建 targets 映射。
        let _ = specs;
    }
}

impl Default for McpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for McpPlugin {
    fn drop(&mut self) {
        // 通知后台 capability 调度器线程停止，避免 plugin 实例被 drop 后线程继续存活。
        // 生产入口（CLI/Tauri/Server）持有 Arc<McpPlugin>，最后一个 Arc 释放时触发。
        self.capability.shutdown();
    }
}

/// MCP 工具仅经 function-calling 暴露给 LLM，无 system prompt 段落注入。
impl tiangong_core::tool_override::PromptSectionProvider for McpPlugin {}

impl Plugin for McpPlugin {
    fn id(&self) -> &str {
        "mcp"
    }

    fn register(&self, _engine: &RuntimeEngine) {
        // 加载 capability 缓存 + 启动后台调度器 + 异步预热 + 重建 targets。
        let _ = self.capability.load_cache(&self.capability_cache_path);
        self.reconfigure();
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }

    fn set_trust_mode(&self, _trust: Arc<RwLock<TrustMode>>) {
        // MCP 工具执行受 engine 层 PermissionGate 统一兜底，无需插件自行感知。
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
    }

    fn collect_exec_env(&self) -> std::collections::BTreeMap<String, String> {
        // 贡献启用 MCP server 的环境变量（API keys 等），供 run_command 子进程注入。
        self.collect_runtime_env()
    }

    fn on_engine_rebuilt(&self, _session: &mut tiangong_core::session::Session) {
        // engine 重建（配置变更）后，capability scheduler 重配 + targets 重建。
        self.reconfigure();
    }
}

/// MCP capability 后台刷新间隔（秒），与原 app_state 常量保持一致。
pub(crate) const MCP_CAPABILITY_REFRESH_INTERVAL_SECS: u64 = 300;

/// 从指定路径加载 MCP 配置；文件不存在或解析失败时返回默认配置。
///
/// 解析失败（JSON 语法错误 / 字段不兼容）时记录 `warn!`，避免静默吞错导致
/// 后续管理操作覆盖用户原配置。读取失败仍返回默认配置以保证可用性。
pub(crate) fn load_mcp_config_from_path(path: &std::path::Path) -> McpConfig {
    if !path.exists() {
        return McpConfig::default();
    }
    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<McpConfig>(&content) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(
                    "MCP 配置解析失败，回退为默认配置（原文件保留，管理操作可能覆盖）：path={} error={err}",
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

/// 把 MCP 配置写入磁盘（带父目录创建）。
pub(crate) fn write_mcp_config_to_path(path: &std::path::Path, config: &McpConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建 MCP 配置目录失败：{}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(config).context("序列化 mcp 配置失败")?;
    std::fs::write(path, content)
        .with_context(|| format!("写入 mcp 配置失败：{}", path.display()))?;
    Ok(())
}
