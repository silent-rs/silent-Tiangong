//! Skill 详情工具规格与覆盖处理器实现。
//!
//! 提供 `get_skill_detail` 工具：从自托管 [`SkillRegistry`] 按 id 加载 skill 的
//! SKILL.md 全文（含 `{skill_dir}` 替换），供 Agent 按需查阅已安装 skill 的使用说明。
//!
//! Skill 的**创建/安装**不经专用工具——由 prompt 段落（见 [`crate::prompt`]）引导
//! Agent 使用通用文件工具在 skills 目录下自行编写 `skill.toml` + `SKILL.md`。

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
        // 仅在存在 available=true 的 skill 时暴露 get_skill_detail。
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

/// 执行 skill 详情查询：从 registry 按 id 加载 SKILL.md 全文，做 `{skill_dir}` 替换。
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
