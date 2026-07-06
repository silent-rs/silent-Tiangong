//! MCP 管理插件——自治状态 + [`Plugin`] trait 实现。
//!
//! 对齐 [`tiangong_plugin_skill::SkillPlugin`] 的自治配方：plugin 自托管
//! [`McpConfig`]（读写 `~/.tiangong/mcp.json`）与 `mcp_targets` 绑定，
//! core 不再持有 MCP 概念。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use tiangong_core::core::Plugin;
use tiangong_core::core::plugin::PluginFeedbackTx;
use tiangong_core::permission::TrustMode;
use tiangong_core::runtime::RuntimeEngine;

use crate::capability::{
    configure_mcp_capability_scheduler, load_mcp_capabilities_cache, refresh_mcp_capabilities_async,
};
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

    /// 用显式路径构造（主要供测试使用）。
    pub fn with_paths(mcp_config_path: PathBuf, capability_cache_path: PathBuf) -> Self {
        let mcp_config = load_mcp_config_from_path(&mcp_config_path);
        Self {
            mcp_config: RwLock::new(mcp_config),
            mcp_targets: RwLock::new(HashMap::new()),
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

    /// 重配 capability scheduler + 重建 mcp_targets 绑定。
    ///
    /// 在 register、config 变更、engine rebuild 时调用，保证 MCP 工具规格
    /// 与 capability 缓存同步。
    pub(crate) fn reconfigure(&self) {
        let config = self.config_snapshot();
        configure_mcp_capability_scheduler(
            config.clone(),
            self.capability_cache_path.clone(),
            MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        refresh_mcp_capabilities_async(config.clone());
        self.rebuild_targets(&config);
    }

    /// 根据当前 capability 缓存重建工具名→目标绑定。
    fn rebuild_targets(&self, config: &McpConfig) {
        // reserved_names 留空：MCP 工具内部的同名冲突由 execution_function_tools
        // 自行处理（mcp__server__tool 前缀），与其他插件工具的冲突在 core/mod.rs
        // 工具汇总阶段通过 reserved_names 过滤（此处 plugin 独立收集不感知其他插件）。
        let (specs, targets) = execution_function_tools(config, std::collections::HashSet::new());
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

/// MCP 工具仅经 function-calling 暴露给 LLM，无 system prompt 段落注入。
impl tiangong_core::tool_override::PromptSectionProvider for McpPlugin {}

impl Plugin for McpPlugin {
    fn id(&self) -> &str {
        "mcp"
    }

    fn register(&self, _engine: &RuntimeEngine) {
        // 加载 capability 缓存 + 启动后台调度器 + 预热 + 重建 targets。
        let _ = load_mcp_capabilities_cache(&self.capability_cache_path);
        self.reconfigure();
    }

    fn set_workspace(&self, workspace: &std::path::Path) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = Some(workspace.to_path_buf());
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

    fn on_engine_rebuilt(&self, _session: &mut tiangong_core::session::Session) {
        // engine 重建（配置变更）后，capability scheduler 重配 + targets 重建。
        self.reconfigure();
    }
}

/// MCP capability 后台刷新间隔（秒），与原 app_state 常量保持一致。
const MCP_CAPABILITY_REFRESH_INTERVAL_SECS: u64 = 300;

/// 从指定路径加载 MCP 配置；文件不存在或解析失败时返回默认配置。
pub(crate) fn load_mcp_config_from_path(path: &std::path::Path) -> McpConfig {
    if !path.exists() {
        return McpConfig::default();
    }
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => McpConfig::default(),
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
