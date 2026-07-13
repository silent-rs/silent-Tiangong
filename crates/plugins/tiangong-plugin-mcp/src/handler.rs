//! MCP 工具规格 + 执行 override 分发。
//!
//! 对齐 [`tiangong_plugin_skill::SkillPlugin`] 的 handler 模式：
//! - [`ToolSpecProvider::tool_specs`]：动态收集 MCP 工具规格
//! - [`ToolOverrideHandler::handle`]：查 `mcp_targets` 分发 MCP 工具调用

use std::future::Future;
use std::pin::Pin;

use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::execution::execute_mcp_tool_call;
use crate::plugin::McpPlugin;

impl McpPlugin {
    /// 分发 MCP 工具调用。返回 `Some(result)` 表示命中 MCP 工具。
    ///
    /// MCP 工具只通过 `mcp__{server}__{tool}` 显式函数名调用（由 tool_overrides
    /// 按 spec.name 注册路由）。run_command/run_shell 的兼容 shim 已移除——
    /// runtime 的 register_tool_override 是 first-writer-wins，command plugin
    /// 先注册，MCP plugin 收不到这两个 tool name，shim 不可达。
    fn dispatch(
        &self,
        call: &ToolCall,
    ) -> Option<Pin<Box<dyn Future<Output = ToolResult> + Send>>> {
        let config = self.config_snapshot();
        let targets = self.targets_snapshot();

        let target = targets.get(&call.name)?.clone();
        let call = call.clone();
        Some(Box::pin(async move {
            match execute_mcp_tool_call(&call, &target, &config).await {
                Ok(result) => result,
                Err(err) => ToolResult {
                    ok: false,
                    summary: format!("MCP工具调用失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: None,
                },
            }
        }))
    }
}

impl ToolSpecProvider for McpPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        let config = self.config_snapshot();
        let active = self.capability.cached_active_tools();
        // reserved_names 留空：MCP 工具规格在 plugin 内独立收集，
        // 与其他插件工具名的冲突消解由 core/mod.rs 工具汇总阶段统一处理。
        let (specs, _) = crate::execution::execution_function_tools(
            &config,
            active,
            std::collections::HashSet::new(),
        );
        specs
    }
}

impl ToolOverrideHandler for McpPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &mut Session,
        _actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        match self.dispatch(call) {
            Some(fut) => Box::pin(async move { Some(fut.await) }),
            None => Box::pin(async { None }),
        }
    }
}
