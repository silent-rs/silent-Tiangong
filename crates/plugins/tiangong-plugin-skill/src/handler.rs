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
const TOOL_INSTALL_SKILL: &str = "install_skill";

impl SkillPlugin {
    /// 主分发入口：同步解析参数并返回 owned Future（借用不逃逸到 async 上下文）。
    fn dispatch(
        &self,
        call: &ToolCall,
        _session: &Session,
    ) -> Option<Pin<Box<dyn Future<Output = ToolResult> + Send>>> {
        match call.name.as_str() {
            TOOL_GET_SKILL_DETAIL => Some(self.handle_get_skill_detail(call)),
            TOOL_INSTALL_SKILL => Some(self.handle_install_skill(call)),
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

    /// 处理 `install_skill` 工具调用。
    ///
    /// 内容式安装：agent 提供 skill 正文，落地为 `~/.tiangong/skills/<id>/`。
    /// 同步文件写入，包装为 owned Future。
    fn handle_install_skill(
        &self,
        call: &ToolCall,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
        let registry = self.registry();
        let result = execute_install_skill(call, &registry);
        Box::pin(async move { result })
    }
}

impl ToolSpecProvider for SkillPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        // install_skill 始终暴露（创作工具，无需已有 skill）。
        let mut specs = vec![
            ToolSpec {
                name: TOOL_INSTALL_SKILL.to_string(),
                description: "安装或更新一个 Skill。通过提供 name、id 和 SKILL.md 正文内容来创建新的技能；若同 id 已存在则覆盖更新。用于让 Agent 自主编写技能或导入外部技能内容。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Skill 唯一标识（小写字母/数字/中横线，将作为目录名）"
                        },
                        "name": {
                            "type": "string",
                            "description": "Skill 显示名称"
                        },
                        "content": {
                            "type": "string",
                            "description": "SKILL.md 正文（Markdown）。首行应为 '# <标题>'，其后是适用场景、使用方式、约束等说明。支持 '{skill_dir}' 占位符（运行时替换为 skill 目录绝对路径）"
                        },
                        "requires_mcp": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "package": { "type": "string" },
                                    "version": { "type": "string" }
                                }
                            },
                            "description": "可选：声明的托管 MCP 依赖"
                        }
                    },
                    "required": ["id", "name", "content"]
                }),
            },
        ];

        // get_skill_detail 仅在存在 available skill 时暴露。
        let view = self.registry().view();
        let has_enabled = view.entries.values().any(|entry| {
            read_skill_manifest(&entry.dir.join("skill.toml"))
                .map(|m| m.available)
                .unwrap_or(false)
        });
        if has_enabled {
            specs.push(ToolSpec {
                name: TOOL_GET_SKILL_DETAIL.to_string(),
                description: "获取已安装 Skill 的完整使用说明".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "skill_id": { "type": "string" }
                    },
                    "required": ["skill_id"]
                }),
            });
        }
        specs
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

/// 执行内容式 skill 安装。
///
/// agent 提供 id / name / content（SKILL.md 正文），落地为 `~/.tiangong/skills/<id>/`。
/// 已存在同 id 则覆盖（re-install）。安装后刷新 registry 缓存。
fn execute_install_skill(
    call: &ToolCall,
    registry: &tiangong_core::skill::SkillRegistry,
) -> ToolResult {
    let raw_id = call
        .arguments
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name = call
        .arguments
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let content = call
        .arguments
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if raw_id.trim().is_empty() {
        return param_error("install_skill 缺少 id 参数");
    }
    if name.trim().is_empty() {
        return param_error("install_skill 缺少 name 参数");
    }
    if content.trim().is_empty() {
        return param_error("install_skill 缺少 content 参数");
    }

    let skill_id = normalize_skill_id(raw_id.trim());
    if skill_id.is_empty() {
        return param_error(&format!("install_skill 的 id 规范化后为空：{raw_id}"));
    }

    // 解析可选的 requires_mcp（用于写入 skill.toml）
    let requires_mcp: Vec<(String, String, String)> = call
        .arguments
        .get("requires_mcp")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let id = item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let pkg = item
                        .get("package")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let ver = item
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if pkg.is_empty() {
                        None
                    } else {
                        Some((id, pkg, ver))
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // 落地到 registry 根目录下的 <id>/（即 ~/.tiangong/skills/<id>/）。
    let skills_root = registry.root().to_path_buf();
    let skill_dir = skills_root.join(&skill_id);

    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        return ToolResult {
            ok: false,
            summary: format!("创建 skill 目录失败：{e}"),
            stdout: String::new(),
            stderr: e.to_string(),
            exit_code: 1,
            execution: None,
        };
    }

    // 写 SKILL.md
    if let Err(e) = std::fs::write(skill_dir.join("SKILL.md"), content) {
        return ToolResult {
            ok: false,
            summary: format!("写入 SKILL.md 失败：{e}"),
            stdout: String::new(),
            stderr: e.to_string(),
            exit_code: 1,
            execution: None,
        };
    }

    // 生成 skill.toml
    let toml_content = build_skill_toml(&skill_id, name.trim(), &requires_mcp);
    if let Err(e) = std::fs::write(skill_dir.join("skill.toml"), toml_content) {
        return ToolResult {
            ok: false,
            summary: format!("写入 skill.toml 失败：{e}"),
            stdout: String::new(),
            stderr: e.to_string(),
            exit_code: 1,
            execution: None,
        };
    }

    // 刷新 registry 缓存，让后续 get_skill_detail / tool_specs / prompt 读到新 skill。
    registry.refresh();

    ToolResult {
        ok: true,
        summary: format!("skill 已安装：{skill_id}"),
        stdout: format!(
            "Skill {name}（id={skill_id}）已安装到 {}\n下次对话生效；如需立即查看说明，调用 get_skill_detail。",
            skill_dir.display()
        ),
        stderr: String::new(),
        exit_code: 0,
        execution: None,
    }
}

/// 规范化 skill id：小写、仅保留 [a-z0-9-]，其余替换为 -，去首尾 -。
fn normalize_skill_id(raw: &str) -> String {
    let mut s: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() && c.is_ascii_lowercase() {
                c
            } else if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.trim_matches('-').to_string()
}

/// 生成 skill.toml 内容。
fn build_skill_toml(
    skill_id: &str,
    skill_name: &str,
    requires_mcp: &[(String, String, String)],
) -> String {
    let mut s = format!(
        "id = \"{skill_id}\"\nname = \"{skill_name}\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\navailable = true\n\n[source]\ntype = \"local\"\nvalue = \"\"\n\n[requires]\n"
    );
    if requires_mcp.is_empty() {
        s.push_str("mcp = []\n");
    } else {
        for (idx, (id, pkg, ver)) in requires_mcp.iter().enumerate() {
            if idx == 0 {
                s.push_str("mcp = [\n");
            }
            s.push_str(&format!(
                "  {{ id = \"{}\", source = \"\", package = \"{}\", version = \"{}\" }},\n",
                id, pkg, ver
            ));
        }
        // 去掉末尾逗号并闭合
        if s.ends_with(",\n") {
            s.truncate(s.len() - 2);
            s.push('\n');
        }
        s.push_str("]\n");
    }
    s.push_str("\n[permissions]\nfs_read = []\nfs_write = []\ncmd_exec = []\nnet = []\n");
    s
}

/// 参数错误结果构造。
fn param_error(msg: &str) -> ToolResult {
    ToolResult {
        ok: false,
        summary: msg.to_string(),
        stdout: String::new(),
        stderr: msg.to_string(),
        exit_code: 1,
        execution: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_skill_id_handles_cases() {
        assert_eq!(normalize_skill_id("My Skill"), "my-skill");
        assert_eq!(normalize_skill_id("foo_bar BAZ"), "foo-bar-baz");
        assert_eq!(normalize_skill_id("  --weird--  "), "weird");
        assert_eq!(normalize_skill_id("中文"), "");
        assert_eq!(normalize_skill_id("a---b"), "a-b");
    }

    #[test]
    fn build_skill_toml_without_mcp() {
        let toml = build_skill_toml("demo", "Demo", &[]);
        assert!(toml.contains("id = \"demo\""));
        assert!(toml.contains("name = \"Demo\""));
        assert!(toml.contains("mcp = []"));
        assert!(toml.contains("available = true"));
    }

    #[test]
    fn build_skill_toml_with_mcp() {
        let toml = build_skill_toml(
            "demo",
            "Demo",
            &[("tool".to_string(), "pkg-a".to_string(), "1.0.0".to_string())],
        );
        assert!(toml.contains("package = \"pkg-a\""));
        assert!(toml.contains("version = \"1.0.0\""));
        assert!(toml.contains("id = \"tool\""));
    }

    #[test]
    fn install_skill_writes_files_and_registry_picks_up() {
        use tiangong_core::skill::scan_skill_registry;
        let tmp = tempfile::tempdir().unwrap();
        let registry = tiangong_core::skill::SkillRegistry::new(tmp.path().to_path_buf());

        // 构造 install_skill 调用
        let call = ToolCall {
            id: "test".to_string(),
            name: "install_skill".to_string(),
            arguments: serde_json::json!({
                "id": "My Test Skill",
                "name": "My Test Skill",
                "content": "# My Test Skill\n\n这是一个测试 skill。"
            }),
        };

        let result = execute_install_skill(&call, &registry);
        assert!(result.ok, "stderr: {}", result.stderr);

        let skill_dir = tmp.path().join("my-test-skill");
        assert!(skill_dir.join("SKILL.md").exists());
        assert!(skill_dir.join("skill.toml").exists());

        // registry 应能扫到
        let view = scan_skill_registry(tmp.path());
        assert!(view.entries.contains_key("my-test-skill"));
    }
}
