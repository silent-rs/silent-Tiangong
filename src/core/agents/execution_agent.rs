use anyhow::{Result, anyhow};

use crate::core::model::{
    FunctionToolSpec, ModelClient, ModelFunctionCall, ModelRequest, SingleProviderClient,
};
use crate::core::planner::{PlanStep, PlanStepStatus, TaskPlan};
use crate::core::session::{Message, Session};
use crate::core::tool::{LocalToolExecutor, ToolCall, ToolExecutor, ToolName, ToolResult};

#[derive(Debug, Clone)]
pub struct ExecutionLlmOutput {
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionStepReport {
    pub step_name: String,
    pub status: PlanStepStatus,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct ExecutionStepResult {
    pub llm_output: Option<ExecutionLlmOutput>,
    pub report: ExecutionStepReport,
}

#[allow(clippy::too_many_arguments)]
pub fn execute_single_plan_step_with_execution_agent(
    client: &SingleProviderClient,
    tool_executor: &LocalToolExecutor,
    session: &Session,
    user_input: &str,
    context: &[Message],
    plan: &TaskPlan,
    plan_name: &str,
    step: &PlanStep,
    previous_plan_summaries: &[String],
    tool_results: &mut Vec<ToolResult>,
) -> Result<ExecutionStepResult> {
    let request = ModelRequest {
        session_title: format!("{} · execution-agent", session.title),
        user_input: build_step_execution_prompt(
            user_input,
            plan,
            plan_name,
            step,
            previous_plan_summaries,
        ),
        context: context.to_vec(),
    };
    let response = client.complete_with_functions(&request, &basic_file_function_tools())?;
    let llm_tool_calls = response
        .tool_calls
        .iter()
        .map(|call| call.name.clone())
        .collect::<Vec<_>>();
    let _ = response.usage.total_tokens;

    let llm_output = if !response.text.trim().is_empty()
        || !response.reasoning_content.trim().is_empty()
        || !llm_tool_calls.is_empty()
    {
        Some(ExecutionLlmOutput {
            content: response.text.clone(),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: llm_tool_calls,
        })
    } else {
        None
    };

    if response.tool_calls.is_empty() {
        return Err(anyhow!(
            "执行智能体未提交任何函数调用：step={} {}",
            step.name,
            step.description
        ));
    }

    let mut step_completed = false;
    let mut executed_tools = Vec::new();
    for tool_call in &response.tool_calls {
        let _function_call_id = tool_call.id.as_str();
        if tool_call.name == "mark_step_completed" {
            step_completed = true;
            continue;
        }
        let call = build_tool_call_from_function(tool_call)?;
        let result = tool_executor.execute(&call)?;
        if let Some(execution) = result.execution.as_ref() {
            executed_tools.push(execution.tool_name.clone());
        }
        tool_results.push(result.clone());
        if !result.ok {
            return Err(anyhow!("{}", build_tool_failure_error(&result)));
        }
    }

    if !step_completed {
        let tool_hint = if executed_tools.is_empty() {
            "未调用任何工具".to_string()
        } else {
            format!("已调用工具：{}", executed_tools.join(","))
        };
        return Err(anyhow!(
            "执行智能体未显式提交步骤完成信号（mark_step_completed）：step={} {}；{}",
            step.name,
            step.description,
            tool_hint
        ));
    }

    let tool_summary = if executed_tools.is_empty() {
        "步骤完成：未调用外部工具".to_string()
    } else {
        format!("步骤完成：工具成功：{}", executed_tools.join(","))
    };

    Ok(ExecutionStepResult {
        llm_output,
        report: ExecutionStepReport {
            step_name: step.name.clone(),
            status: PlanStepStatus::Completed,
            summary: tool_summary,
        },
    })
}

fn build_tool_failure_error(result: &ToolResult) -> String {
    let tool_name = result
        .execution
        .as_ref()
        .map(|record| record.tool_name.as_str())
        .unwrap_or("unknown");
    let stderr_line = result
        .stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .to_string();
    if stderr_line.is_empty() {
        format!("工具调用失败：tool={tool_name}，summary={}", result.summary)
    } else {
        format!(
            "工具调用失败：tool={tool_name}，summary={}，stderr={stderr_line}",
            result.summary
        )
    }
}

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

fn build_step_execution_prompt(
    user_input: &str,
    plan: &TaskPlan,
    plan_name: &str,
    step: &PlanStep,
    previous_plan_summaries: &[String],
) -> String {
    let plan_snapshot = format_plan_snapshot(plan);
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
    format!(
        r#"你是执行智能体，负责执行当前 plan 步骤并确保结果可落地。

约束：
1. 只围绕“当前步骤”执行，不要改写 plan，不要新增步骤。
2. 需要文件/命令操作时，必须调用可用工具完成。
3. 优先使用 `search_code` 与 `read_file` 先定位再修改，避免盲改文件。
4. 使用 `apply_patch` 时优先采用 Codex 风格补丁（*** Begin Patch ... *** End Patch）。
5. 完成步骤时，必须调用 `mark_step_completed` 函数作为完成信号。
6. 若调用了工具，`mark_step_completed` 必须在所有工具调用成功后再调用。
7. 不要输出冗长解释，聚焦执行结果。

用户输入：
{user_input}

当前 plan：
{plan_name}

已完成 plan 的执行汇总（仅供参考）：
{previous_plan_result_text}

当前计划快照：
{plan_snapshot}

当前步骤：
- name: {step_name}
- description: {step_desc}"#,
        plan_name = plan_name,
        previous_plan_result_text = previous_plan_result_text,
        step_name = step.name,
        step_desc = step.description
    )
}

fn basic_file_function_tools() -> Vec<FunctionToolSpec> {
    vec![
        FunctionToolSpec {
            name: "list_dir".to_string(),
            description: "列出目录中的文件和子目录".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径，默认当前目录" }
                },
                "required": []
            }),
        },
        FunctionToolSpec {
            name: "tree_dir".to_string(),
            description: "按目录树格式列出目录，支持通过 max_depth 限制遍历深度".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径，默认当前目录" },
                    "max_depth": {
                        "type": "integer",
                        "description": "遍历最大深度，建议 1-4，默认 2，最大 8",
                        "minimum": 0,
                        "maximum": 8
                    }
                },
                "required": []
            }),
        },
        FunctionToolSpec {
            name: "read_file".to_string(),
            description: "读取文件内容".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" }
                },
                "required": ["path"]
            }),
        },
        FunctionToolSpec {
            name: "search_code".to_string(),
            description: "在目录中检索文本（优先使用 rg）".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "检索文本或正则模式" },
                    "path": { "type": "string", "description": "目标目录或文件路径，默认当前目录" }
                },
                "required": ["pattern"]
            }),
        },
        FunctionToolSpec {
            name: "write_file".to_string(),
            description: "创建或覆盖文件内容".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "content": { "type": "string", "description": "要写入的完整内容" }
                },
                "required": ["path", "content"]
            }),
        },
        FunctionToolSpec {
            name: "replace_in_file".to_string(),
            description: "在文件中将旧文本替换为新文本".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "old": { "type": "string", "description": "待替换的旧文本" },
                    "new": { "type": "string", "description": "替换后的新文本" }
                },
                "required": ["path", "old", "new"]
            }),
        },
        FunctionToolSpec {
            name: "run_command".to_string(),
            description: "执行受控命令，参数为命令名与参数数组。bash 请使用 cmd=bash,args=[\"-lc\",\"<script>\"]".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "命令名，例如 ls/cat/echo/pwd/bash" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "命令参数列表"
                    }
                },
                "required": ["cmd"]
            }),
        },
        FunctionToolSpec {
            name: "apply_patch".to_string(),
            description: "对文件应用补丁文本，支持 Codex 风格补丁（*** Begin Patch ... *** End Patch）".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "补丁内容文本" }
                },
                "required": ["patch"]
            }),
        },
        FunctionToolSpec {
            name: "mark_step_completed".to_string(),
            description: "标记当前执行步骤已完成。仅在本步骤真正完成后调用。".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "result": { "type": "string", "description": "本步骤完成结果摘要" }
                },
                "required": []
            }),
        },
    ]
}

fn build_tool_call_from_function(call: &ModelFunctionCall) -> Result<ToolCall> {
    let mut args = Vec::new();
    let tool_call = match call.name.as_str() {
        "list_dir" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            args.push(path);
            Ok(ToolCall {
                name: ToolName::ListDir,
                args,
            })
        }
        "tree_dir" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            let max_depth = call
                .arguments
                .get("max_depth")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value.to_string())
                .or_else(|| {
                    call.arguments
                        .get("max_depth")
                        .and_then(serde_json::Value::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| "2".to_string());
            args.push(path);
            args.push(max_depth);
            Ok(ToolCall {
                name: ToolName::TreeDir,
                args,
            })
        }
        "read_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            Ok(ToolCall {
                name: ToolName::ReadFile,
                args,
            })
        }
        "write_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let content = call
                .arguments
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            args.push(content);
            Ok(ToolCall {
                name: ToolName::WriteFile,
                args,
            })
        }
        "search_code" => {
            let pattern = call
                .arguments
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".")
                .to_string();
            args.push(pattern);
            args.push(path);
            Ok(ToolCall {
                name: ToolName::SearchCode,
                args,
            })
        }
        "replace_in_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let old = call
                .arguments
                .get("old")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let new = call
                .arguments
                .get("new")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(path);
            args.push(old);
            args.push(new);
            Ok(ToolCall {
                name: ToolName::ReplaceInFile,
                args,
            })
        }
        "run_command" => {
            let cmd = call
                .arguments
                .get("cmd")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(cmd);
            if let Some(arr) = call
                .arguments
                .get("args")
                .and_then(serde_json::Value::as_array)
            {
                args.extend(
                    arr.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(ToString::to_string),
                );
            }
            Ok(ToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        "run_bash" => {
            let script = call
                .arguments
                .get("script")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push("bash".to_string());
            args.push("-lc".to_string());
            args.push(script);
            Ok(ToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        "apply_patch" => {
            let patch = call
                .arguments
                .get("patch")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            args.push(patch);
            Ok(ToolCall {
                name: ToolName::ApplyPatch,
                args,
            })
        }
        _ => Err(anyhow!("未知函数调用：{}", call.name)),
    }?;
    Ok(tool_call)
}
