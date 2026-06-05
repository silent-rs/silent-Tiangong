use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::planner::{PlanStep, PlanStepStatus, TaskPlan};

use super::execution_mcp_agent::McpFunctionTarget;

fn plan_step_status_label(status: PlanStepStatus) -> &'static str {
    match status {
        PlanStepStatus::Pending => "PENDING",
        PlanStepStatus::Completed => "DONE",
        PlanStepStatus::Failed => "FAILED",
        PlanStepStatus::Ignored => "IGNORED",
    }
}

fn format_plan_snapshot(plan: &TaskPlan) -> String {
    let mut lines = Vec::new();
    lines.push(format!("objective: {}", plan.objective));
    for (plan_idx, item) in plan.plans.iter().enumerate() {
        lines.push(format!(
            "P{} [{}] {} - {}",
            plan_idx + 1,
            plan_step_status_label(item.status),
            item.name,
            item.description
        ));
        if let Some(summary) = item.execution_summary.as_ref() {
            lines.push(format!("  RESULT: {}", summary.replace('\n', " | ")));
        }
        for (step_idx, step) in item.execution_steps.iter().enumerate() {
            lines.push(format!(
                "  S{}.{} [{}] {} - {}",
                plan_idx + 1,
                step_idx + 1,
                plan_step_status_label(step.status),
                step.name,
                step.description
            ));
        }
    }
    lines.join("\n")
}

pub(crate) fn build_step_execution_prompt(
    user_input: &str,
    plan: &TaskPlan,
    plan_name: &str,
    step: &PlanStep,
    previous_plan_summaries: &[String],
    mcp_function_targets: &HashMap<String, McpFunctionTarget>,
) -> String {
    let plan_snapshot = format_plan_snapshot(plan);
    let skill_context = build_skill_context(plan);
    let previous_plan_result_text = if previous_plan_summaries.is_empty() {
        "无".to_string()
    } else {
        previous_plan_summaries
            .iter()
            .enumerate()
            .map(|(idx, summary)| format!("{}. {}", idx + 1, summary))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mcp_function_names_text = format_mcp_function_name_hint(mcp_function_targets);
    let skills_dir = default_skills_dir_display();
    format!(
        r#"你是执行智能体，负责执行当前 plan 步骤并确保结果可落地。

约束：
1. 只围绕“当前步骤”执行，不要改写 plan，不要新增步骤。
2. 需要文件/命令操作时，必须调用可用工具完成。
3. 优先使用 `search_code` 与 `read_file` 先定位再修改，避免盲改文件。
4. 使用 `apply_patch` 时仅使用天工补丁路线（unified diff，含 ---/+++/@@）。
5. 完成步骤时，必须调用 `mark_step_completed` 函数作为完成信号。
6. 若调用了工具，`mark_step_completed` 必须在所有工具调用成功后再调用。
6.1 `mark_step_completed` 必须显式传入 `continue_execution`；当需要继续时，必须提供 `next_step_name` 与 `next_step_description`。
7. 不要输出冗长解释，聚焦执行结果。
8. 若命中 Skills，优先遵循 Skill 指令；如需运行脚本，优先在对应 Skill 目录（cwd）执行。
9. 当 Skill 提供可直接执行的命令时，不要只停留在目录浏览，必须尝试实际调用。
10. Skill/MCP 相关环境变量已由运行时注入，不要执行 `env` / `printenv` / `set` / `grep` 等环境探测命令。
11. 不要把管道或复合命令写进 `run_command.cmd`（如 `env | grep DINGTALK`）；需要脚本时使用 `run_shell`。
12. 若命中 MCP，上下文已提供可用目标，优先按 MCP 上下文执行，不要用环境探测替代真实调用。
12.1 绝对不要把 MCP 工具名当成 shell 命令写进 `run_command`（例如 `list_categories`）；应直接调用同名函数工具。
12.2 可用 MCP 函数名列表已提供；调用时必须精确使用这些函数名。
13. 若配置缺失，直接执行目标业务命令并根据错误回传定位问题，不要先做环境枚举。
14. 当前步骤支持多轮执行：若本轮尚未完成，请继续调用工具；最终必须调用 `mark_step_completed` 结束该步骤。
15. 当工具返回成功输出时，应先判断是否已满足用户请求；若仅是中间结果，调用 `mark_step_completed(continue_execution=true)` 并提供下一步。
16. 当用户要求创建或安装 Skill 时，使用 `write_file` 将文件写入 Skill 目录（{skills_dir}/<skill-id>/）。每个 Skill 必须包含 skill.toml（清单）和 SKILL.md（入口文档），可选包含脚本等附加文件。skill.toml 格式：id（小写字母数字短横线）、name（显示名）、version、entry="SKILL.md"、available=true、[source] type="agent"、[requires] mcp=[]、[permissions]（fs_read/fs_write/cmd_exec/net 按需声明）。创建完成后告知用户刷新 Skill 列表即可使用。
17. 当用户要求用浏览器打开页面或本地 HTML 文件时，必须使用 `web_fetch`（传入 file:// 路径或 URL），不要使用 `run_shell` 调用系统浏览器（如 open、xdg-open）。

用户输入：
{user_input}

当前 plan：
{plan_name}

已完成 plan 的执行汇总（仅供参考）：
{previous_plan_result_text}

当前计划快照：
{plan_snapshot}

命中 Skill 上下文：
{skill_context}

当前可用 MCP 函数名：
{mcp_function_names_text}

当前步骤：
- name: {step_name}
- description: {step_desc}"#,
        plan_name = plan_name,
        previous_plan_result_text = previous_plan_result_text,
        skill_context = skill_context,
        mcp_function_names_text = mcp_function_names_text,
        skills_dir = skills_dir,
        step_name = step.name,
        step_desc = step.description
    )
}

pub(crate) fn build_step_followup_prompt(step: &PlanStep, round: usize) -> String {
    format!(
        r#"继续执行同一个步骤（第 {round} 轮）。

步骤信息：
- name: {step_name}
- description: {step_desc}

要求：
1. 基于“上一轮工具执行结果”继续推进，不能停留在目录浏览。
2. 若步骤已完成，必须调用 `mark_step_completed`。
3. 若步骤未完成，继续调用必要工具；当确认完成后再调用 `mark_step_completed`。
4. 若需继续后续动态步骤，`mark_step_completed` 必须包含 `continue_execution=true` 和下一步信息。
5. 若上一轮已出现业务成功结果（例如 JSON `success=true`），本轮禁止再尝试其他命令，直接调用 `mark_step_completed`。"#,
        step_name = step.name,
        step_desc = step.description
    )
}

fn format_mcp_function_name_hint(targets: &HashMap<String, McpFunctionTarget>) -> String {
    if targets.is_empty() {
        return "无".to_string();
    }
    let mut names = targets.keys().cloned().collect::<Vec<_>>();
    names.sort();
    const MAX_HINT_NAMES: usize = 40;
    let mut parts = names
        .iter()
        .take(MAX_HINT_NAMES)
        .cloned()
        .collect::<Vec<_>>();
    if names.len() > MAX_HINT_NAMES {
        parts.push(format!("...(省略 {} 项)", names.len() - MAX_HINT_NAMES));
    }
    parts.join(", ")
}

fn build_skill_context(plan: &TaskPlan) -> String {
    if plan.skill_hints.is_empty() {
        return "无".to_string();
    }
    let mut lines = Vec::new();
    for hint in &plan.skill_hints {
        let Some(skill_ref) = parse_skill_hint(hint) else {
            lines.push(format!("- {hint}"));
            continue;
        };
        let preview = read_skill_preview(&skill_ref.path);
        lines.push(format!(
            "- name: {}\n  path: {}\n  preview:\n{}",
            skill_ref.name,
            skill_ref.path.display(),
            indent_multiline(&preview, 4)
        ));
    }
    lines.join("\n")
}

#[derive(Debug)]
struct SkillHintRef {
    name: String,
    path: PathBuf,
}

fn parse_skill_hint(raw: &str) -> Option<SkillHintRef> {
    let mut name = None;
    let mut path = None;
    for part in raw.split('|') {
        if let Some(value) = part.strip_prefix("name=") {
            let value = value.trim();
            if !value.is_empty() {
                name = Some(value.to_string());
            }
        } else if let Some(value) = part.strip_prefix("detail=") {
            let value = value.trim();
            if !value.is_empty() {
                path = Some(PathBuf::from(value));
            }
        }
    }
    Some(SkillHintRef {
        name: name?,
        path: path?,
    })
}

fn read_skill_preview(path: &Path) -> String {
    let skill_md_path = if path.is_file() {
        path.to_path_buf()
    } else {
        path.join("SKILL.md")
    };
    if !skill_md_path.exists() {
        return "(未找到 SKILL.md)".to_string();
    }
    let raw = match fs::read_to_string(&skill_md_path) {
        Ok(text) => text,
        Err(err) => return format!("(读取 SKILL.md 失败: {err})"),
    };
    if raw.trim().is_empty() {
        "(SKILL.md 为空)".to_string()
    } else {
        raw
    }
}

fn indent_multiline(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{pad}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn default_skills_dir_display() -> String {
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from)
        })
        .unwrap_or_else(|| std::path::PathBuf::from("~"));
    home.join(".tiangong/skills").display().to_string()
}
