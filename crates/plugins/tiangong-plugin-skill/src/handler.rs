//! Skill 详情工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `get_skill_detail` 工具。
//! 工具规格仅当 registry 中存在启用（available）skill 时返回。
//!
//! 参数直接从 LLM 传入的命名参数 JSON（`call.arguments`）按 key 取 `skill_id`。
//! 查询逻辑：从自托管 [`SkillRegistry`] 查找 skill → 读 SKILL.md + `{skill_dir}`
//! 替换 → 错误路径（未找到 / 读取失败）+ 成功路径。
//!
//! 注意：`missing_managed_mcp_servers` 检查已简化——skills 自治后 plugin 不持有
//! mcp 配置，暂不阻断缺少托管 MCP 的 skill（后续优化为经 trait 注入 mcp 快照）。

use std::future::Future;
use std::pin::Pin;

use serde_json::json;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::skill::{read_skill_manifest, scan_skill_registry};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::plugin::SkillPlugin;

/// 工具名常量。
const TOOL_GET_SKILL_DETAIL: &str = "get_skill_detail";

impl SkillPlugin {
    /// 主分发入口：同步解析参数并返回 owned Future（借用不逃逸到 async 上下文）。
    fn dispatch(
        &self,
        call: &ToolCall,
        _session: &Session,
    ) -> Option<Pin<Box<dyn Future<Output = ToolResult> + Send>>> {
        match call.name.as_str() {
            TOOL_GET_SKILL_DETAIL => Some(self.handle_get_skill_detail(call)),
            _ => None,
        }
    }

    /// 处理 `get_skill_detail` 工具调用。
    ///
    /// 同步执行（纯文件读取 + registry 查找，无 async 操作），包装为 owned Future。
    fn handle_get_skill_detail(
        &self,
        call: &ToolCall,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        let registry = self.registry();
        let result = execute_get_skill_detail(call, &registry);
        Box::pin(async move { result })
    }
}

impl ToolSpecProvider for SkillPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        // 从 registry 扫描，存在 available=true 的 skill 才暴露工具。
        let view = self.registry().view();
        let has_enabled = view.entries.values().any(|entry| {
            read_skill_manifest(&entry.dir.join("skill.toml"))
                .map(|m| m.available)
                .unwrap_or(false)
        });
        if !has_enabled {
            return Vec::new();
        }
        vec![ToolSpec {
            name: TOOL_GET_SKILL_DETAIL.to_string(),
            description: "获取已安装 Skill 的完整使用说明".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "skill_id": { "type": "string" }
                },
                "required": ["skill_id"]
            }),
        }]
    }
}

impl ToolOverrideHandler for SkillPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &Session,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        match self.dispatch(call, session) {
            Some(future) => Box::pin(async move { Some(future.await) }),
            None => Box::pin(async { None }),
        }
    }
}

/// 执行 skill 详情查询。
///
/// 从 registry 按 id 加载 skill（含 SKILL.md 全文），做 `{skill_dir}` 替换后返回。
/// 未找到时列出可用的 enabled skill id。
fn execute_get_skill_detail(
    call: &ToolCall,
    registry: &tiangong_core::skill::SkillRegistry,
) -> ToolResult {
    let skill_id = call
        .arguments
        .get("skill_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // 尝试从 registry 加载完整 skill（含 SKILL.md）。
    match registry.get(skill_id) {
        Ok(loaded) => {
            let skill_dir = registry.root().join(skill_id);
            let resolved = loaded
                .readme
                .replace("{skill_dir}", &skill_dir.display().to_string());
            ToolResult {
                ok: true,
                summary: format!("Skill {} 的使用说明", loaded.manifest.name),
                stdout: resolved,
                stderr: String::new(),
                exit_code: 0,
                execution: None,
            }
        }
        Err(_) => {
            // 列出可用的 enabled skill id（从 registry 扫描）。
            let view = scan_skill_registry(registry.root());
            let available: Vec<&str> = view
                .entries
                .values()
                .filter_map(|entry| {
                    read_skill_manifest(&entry.dir.join("skill.toml"))
                        .ok()
                        .filter(|m| m.available)
                        .map(|_| entry.id.as_str())
                })
                .collect();
            ToolResult {
                ok: false,
                summary: format!("未找到 skill：{skill_id}"),
                stdout: String::new(),
                stderr: format!("可用的 skill：{}", available.join(", ")),
                exit_code: 1,
                execution: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // missing_managed_mcp_servers 已随 skills 自治重构移除（mcp 配置不在 plugin 范围）。
    // 原有两个测试随函数一起删除，后续如需 mcp 依赖检查，经 trait 注入 mcp 快照后补回。
}
