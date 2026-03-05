use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::core::model::{
    FunctionToolSpec, ModelClient, ModelFunctionCall, ModelRequest, SingleProviderClient,
};
use crate::core::planner::{PlanStep, PlanStepStatus, TaskPlan};
use crate::core::session::{Message, MessageRole, Session, now_text};
use crate::core::tool::{LocalToolExecutor, ToolCall, ToolExecutor, ToolName, ToolResult};

const INTERNAL_SHELL_CMD: &str = "__tiangong_shell__";
const INTERNAL_CWD_PREFIX: &str = "__tiangong_cwd=";
const MAX_EXECUTION_AGENT_ROUNDS: usize = 6;
const SUCCESS_RESULT_PREVIEW_MAX_CHARS: usize = 500;

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
    pub next_step: Option<DynamicPlanStep>,
}

#[derive(Debug, Clone)]
pub struct DynamicPlanStep {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
struct SuccessfulBusinessResult {
    summary: String,
    payload: Option<Value>,
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
    let step_prompt =
        build_step_execution_prompt(user_input, plan, plan_name, step, previous_plan_summaries);
    let mut loop_context = context.to_vec();
    let mut output_contents = Vec::new();
    let mut output_reasonings = Vec::new();
    let mut output_tool_calls = Vec::new();
    let mut executed_tools = Vec::new();

    for round in 0..MAX_EXECUTION_AGENT_ROUNDS {
        let round_prompt = if round == 0 {
            step_prompt.clone()
        } else {
            build_step_followup_prompt(step, round + 1)
        };
        let request = ModelRequest {
            session_title: format!("{} · execution-agent", session.title),
            user_input: round_prompt,
            context: loop_context.clone(),
        };
        let response = client.complete_with_functions(&request, &basic_file_function_tools())?;
        let _ = response.usage.total_tokens;
        collect_llm_output(
            round + 1,
            &response,
            &mut output_contents,
            &mut output_reasonings,
            &mut output_tool_calls,
        );

        if response.tool_calls.is_empty() {
            return Err(anyhow!(
                "执行智能体未提交任何函数调用：step={} {}，round={}",
                step.name,
                step.description,
                round + 1
            ));
        }

        let mut completion_signal = None;
        let mut successful_result: Option<SuccessfulBusinessResult> = None;
        let mut ignored_failed_tool = false;
        let mut round_feedback = Vec::new();
        for tool_call in &response.tool_calls {
            let _function_call_id = tool_call.id.as_str();
            if tool_call.name == "mark_step_completed" {
                if completion_signal.is_some() {
                    return Err(anyhow!(
                        "重复调用 mark_step_completed：step={} {}，round={}",
                        step.name,
                        step.description,
                        round + 1
                    ));
                }
                completion_signal = Some(parse_completion_signal(tool_call)?);
                continue;
            }
            let call = build_tool_call_from_function(tool_call)?;
            let result = tool_executor.execute(&call)?;
            if let Some(execution) = result.execution.as_ref() {
                executed_tools.push(execution.tool_name.clone());
            }
            if successful_result.is_none() {
                successful_result = extract_successful_business_result(&result);
            }
            round_feedback.push(summarize_round_tool_feedback(&result));
            tool_results.push(result.clone());
            if !result.ok {
                if successful_result.is_some() {
                    ignored_failed_tool = true;
                    round_feedback.push(format!(
                        "检测到明确成功输出后出现额外失败调用，忽略该失败并收敛当前步骤：{}",
                        build_tool_failure_error(&result)
                    ));
                    continue;
                }
                return Err(anyhow!("{}", build_tool_failure_error(&result)));
            }
        }

        if let Some(completion_signal) = completion_signal {
            if completion_signal.continue_execution
                && (completion_signal.next_step_name.is_empty()
                    || completion_signal.next_step_description.is_empty())
            {
                return Err(anyhow!(
                    "mark_step_completed 标记继续执行时必须提供 next_step_name/next_step_description：step={} {}",
                    step.name,
                    step.description,
                ));
            }

            let mut summary_parts = Vec::new();
            if !completion_signal.result.trim().is_empty() {
                summary_parts.push(format!("result={}", completion_signal.result.trim()));
            }
            if executed_tools.is_empty() {
                summary_parts.push("tools=none".to_string());
            } else {
                summary_parts.push(format!("tools={}", executed_tools.join(",")));
            }
            if completion_signal.continue_execution {
                summary_parts.push(format!(
                    "next={} - {}",
                    completion_signal.next_step_name, completion_signal.next_step_description
                ));
            }
            let tool_summary = format!("步骤完成：{}", summary_parts.join("；"));

            let llm_output =
                build_aggregated_llm_output(output_contents, output_reasonings, output_tool_calls);

            return Ok(ExecutionStepResult {
                llm_output,
                report: ExecutionStepReport {
                    step_name: step.name.clone(),
                    status: PlanStepStatus::Completed,
                    summary: tool_summary,
                },
                next_step: completion_signal
                    .continue_execution
                    .then_some(DynamicPlanStep {
                        name: completion_signal.next_step_name,
                        description: completion_signal.next_step_description,
                    }),
            });
        }

        if let Some(successful_result) = successful_result {
            let mut auto_signal = infer_completion_signal_with_llm(
                client,
                session,
                &loop_context,
                user_input,
                plan_name,
                step,
                &successful_result,
                &round_feedback,
            )?;
            if !auto_signal.continue_execution {
                auto_signal = review_completion_signal_with_llm(
                    client,
                    session,
                    &loop_context,
                    user_input,
                    plan_name,
                    step,
                    &successful_result,
                    &round_feedback,
                    &auto_signal,
                )?;
            }
            let mut summary_parts = Vec::new();
            if !auto_signal.result.trim().is_empty() {
                summary_parts.push(format!("result={}", auto_signal.result.trim()));
            } else {
                summary_parts.push(format!("result={}", successful_result.summary));
            }
            if executed_tools.is_empty() {
                summary_parts.push("tools=none".to_string());
            } else {
                summary_parts.push(format!("tools={}", executed_tools.join(",")));
            }
            if ignored_failed_tool {
                summary_parts.push("ignored_failed_tool=true".to_string());
            }
            let next_step = auto_signal.continue_execution.then_some(DynamicPlanStep {
                name: auto_signal.next_step_name.clone(),
                description: auto_signal.next_step_description.clone(),
            });
            if next_step.is_some() {
                summary_parts.push("auto_continue=true".to_string());
            }
            summary_parts.push("auto_decision=llm".to_string());
            let tool_summary = format!("步骤完成（LLM决策收敛）：{}", summary_parts.join("；"));

            let llm_output =
                build_aggregated_llm_output(output_contents, output_reasonings, output_tool_calls);

            return Ok(ExecutionStepResult {
                llm_output,
                report: ExecutionStepReport {
                    step_name: step.name.clone(),
                    status: PlanStepStatus::Completed,
                    summary: tool_summary,
                },
                next_step,
            });
        }

        if round_feedback.is_empty() {
            return Err(anyhow!(
                "执行智能体未显式提交步骤完成信号且未执行工具：step={} {}，round={}",
                step.name,
                step.description,
                round + 1
            ));
        }

        loop_context.push(runtime_message(
            MessageRole::System,
            format!(
                "execution-agent round {} 工具执行结果：\n{}",
                round + 1,
                round_feedback
                    .iter()
                    .enumerate()
                    .map(|(idx, item)| format!("{}. {item}", idx + 1))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        ));
    }

    Err(anyhow!(
        "执行智能体在 {} 轮内未提交步骤完成信号（mark_step_completed）：step={} {}",
        MAX_EXECUTION_AGENT_ROUNDS,
        step.name,
        step.description
    ))
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

fn build_step_followup_prompt(step: &PlanStep, round: usize) -> String {
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

fn collect_llm_output(
    round: usize,
    response: &crate::core::model::ModelFunctionResponse,
    output_contents: &mut Vec<String>,
    output_reasonings: &mut Vec<String>,
    output_tool_calls: &mut Vec<String>,
) {
    let text = response.text.trim();
    if !text.is_empty() {
        output_contents.push(format!("[round {round}] {text}"));
    }

    let reasoning = response.reasoning_content.trim();
    if !reasoning.is_empty() {
        output_reasonings.push(format!("[round {round}] {reasoning}"));
    }

    for call in &response.tool_calls {
        output_tool_calls.push(format!("round{round}:{}", call.name));
    }
}

fn build_aggregated_llm_output(
    output_contents: Vec<String>,
    output_reasonings: Vec<String>,
    output_tool_calls: Vec<String>,
) -> Option<ExecutionLlmOutput> {
    if output_contents.is_empty() && output_reasonings.is_empty() && output_tool_calls.is_empty() {
        return None;
    }
    Some(ExecutionLlmOutput {
        content: output_contents.join("\n"),
        reasoning_content: output_reasonings.join("\n"),
        tool_calls: output_tool_calls,
    })
}

fn summarize_round_tool_feedback(result: &ToolResult) -> String {
    let tool_name = result
        .execution
        .as_ref()
        .map(|record| record.tool_name.as_str())
        .unwrap_or("unknown");
    let mut parts = vec![
        format!("tool={tool_name}"),
        format!("ok={}", result.ok),
        format!("exit_code={}", result.exit_code),
        format!("summary={}", result.summary),
    ];
    if !result.ok {
        let stderr_line = result
            .stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim();
        if !stderr_line.is_empty() {
            parts.push(format!("stderr={stderr_line}"));
        }
    }
    parts.join(" | ")
}

fn extract_successful_business_result(result: &ToolResult) -> Option<SuccessfulBusinessResult> {
    if !result.ok {
        return None;
    }
    let tool_name = result
        .execution
        .as_ref()
        .map(|record| record.tool_name.as_str())
        .unwrap_or_default();
    if tool_name != "run_command" {
        return None;
    }

    let stdout = result.stdout.trim();
    if stdout.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<Value>(stdout)
        && value
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        let compact = serde_json::to_string(&value)
            .unwrap_or_else(|_| truncate_summary_text(stdout, SUCCESS_RESULT_PREVIEW_MAX_CHARS));
        return Some(SuccessfulBusinessResult {
            summary: format!(
                "run_command 返回 success=true: {}",
                truncate_summary_text(&compact, SUCCESS_RESULT_PREVIEW_MAX_CHARS)
            ),
            payload: Some(value),
        });
    }

    let normalized = stdout.replace(' ', "");
    if normalized.contains("\"success\":true") {
        return Some(SuccessfulBusinessResult {
            summary: format!(
                "run_command 输出包含 success=true: {}",
                truncate_summary_text(stdout, SUCCESS_RESULT_PREVIEW_MAX_CHARS)
            ),
            payload: None,
        });
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn infer_completion_signal_with_llm(
    client: &SingleProviderClient,
    session: &Session,
    context: &[Message],
    user_input: &str,
    plan_name: &str,
    step: &PlanStep,
    successful_result: &SuccessfulBusinessResult,
    round_feedback: &[String],
) -> Result<CompletionSignal> {
    let result_payload = successful_result
        .payload
        .as_ref()
        .and_then(|value| serde_json::to_string_pretty(value).ok())
        .unwrap_or_else(|| successful_result.summary.clone());
    let feedback_text = if round_feedback.is_empty() {
        "无".to_string()
    } else {
        round_feedback.join("\n")
    };

    let prompt = format!(
        r#"你是执行步骤完成判定器，需要判断“当前步骤”是否已经满足用户目标，或需要继续补充下一步。

请严格只输出 JSON 对象（不要 markdown，不要代码块）：
{{
  "result": "string，当前步骤结果摘要",
  "continue_execution": false,
  "next_step_name": "",
  "next_step_description": ""
}}

规则：
1. 仅基于提供的用户请求、当前步骤、工具结果做判断。
2. 若当前结果已满足用户请求，continue_execution=false。
3. 若当前结果仅为中间结果或不满足用户请求，continue_execution=true，并给出清晰的下一步名称与描述。
4. 不要建议环境探测（env/printenv/set/grep），直接给出业务下一步。
5. 用户请求若包含“详细信息/详情/完整信息/detail/details”，且结果只包含标识符（如 userId）而无详情字段，必须 continue_execution=true。

用户请求：
{user_input}

当前 plan：
{plan_name}

当前步骤：
- name: {step_name}
- description: {step_desc}

本轮工具反馈：
{feedback_text}

已识别的成功结果：
{result_payload}"#,
        step_name = step.name,
        step_desc = step.description
    );

    let request = ModelRequest {
        session_title: format!("{} · execution-step-evaluator", session.title),
        user_input: prompt,
        context: context.to_vec(),
    };
    let response = client.complete(&request)?;
    let payload = parse_json_object_from_text(&response.text)?;
    parse_completion_signal_from_json(&payload, successful_result.summary.as_str())
}

#[allow(clippy::too_many_arguments)]
fn review_completion_signal_with_llm(
    client: &SingleProviderClient,
    session: &Session,
    context: &[Message],
    user_input: &str,
    plan_name: &str,
    step: &PlanStep,
    successful_result: &SuccessfulBusinessResult,
    round_feedback: &[String],
    proposed_signal: &CompletionSignal,
) -> Result<CompletionSignal> {
    let result_payload = successful_result
        .payload
        .as_ref()
        .and_then(|value| serde_json::to_string_pretty(value).ok())
        .unwrap_or_else(|| successful_result.summary.clone());
    let feedback_text = if round_feedback.is_empty() {
        "无".to_string()
    } else {
        round_feedback.join("\n")
    };
    let proposed_json = serde_json::json!({
        "result": proposed_signal.result,
        "continue_execution": proposed_signal.continue_execution,
        "next_step_name": proposed_signal.next_step_name,
        "next_step_description": proposed_signal.next_step_description,
    });

    let prompt = format!(
        r#"你是执行步骤完成“复核器”，需要严格检查“拟定完成判定”是否真正满足用户请求。

请仅输出 JSON 对象：
{{
  "result": "string",
  "continue_execution": false,
  "next_step_name": "",
  "next_step_description": ""
}}

复核规则：
1. 用户若要求“详细信息/详情/完整信息/detail/details”，结果必须包含能支撑“详细”的字段集合，而非仅标识符（如仅 userId）。
2. 若拟定判定不充分，必须改为 continue_execution=true，并给出下一步名称与描述。
3. 仅基于证据复核，不要编造数据。

用户请求：
{user_input}

当前 plan：
{plan_name}

当前步骤：
- name: {step_name}
- description: {step_desc}

工具反馈：
{feedback_text}

成功结果：
{result_payload}

拟定判定：
{proposed_json}"#,
        step_name = step.name,
        step_desc = step.description
    );

    let request = ModelRequest {
        session_title: format!("{} · execution-step-reviewer", session.title),
        user_input: prompt,
        context: context.to_vec(),
    };
    let response = client.complete(&request)?;
    let payload = parse_json_object_from_text(&response.text)?;
    parse_completion_signal_from_json(&payload, proposed_signal.result.as_str())
}

fn parse_json_object_from_text(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("completion evaluator 返回空内容"));
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }
    let start = trimmed
        .find('{')
        .ok_or_else(|| anyhow!("completion evaluator 输出缺少 JSON 对象起始符"))?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| anyhow!("completion evaluator 输出缺少 JSON 对象结束符"))?;
    let candidate = &trimmed[start..=end];
    serde_json::from_str::<Value>(candidate)
        .map_err(|err| anyhow!("completion evaluator 输出 JSON 解析失败：{err}"))
}

fn parse_completion_signal_from_json(
    value: &Value,
    default_result: &str,
) -> Result<CompletionSignal> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("completion evaluator 输出不是 JSON 对象"))?;
    let result = obj
        .get("result")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(default_result)
        .to_string();
    let continue_execution = obj
        .get("continue_execution")
        .and_then(Value::as_bool)
        .or_else(|| {
            obj.get("continue_execution")
                .and_then(Value::as_str)
                .and_then(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => Some(true),
                    "false" | "0" | "no" | "off" => Some(false),
                    _ => None,
                })
        })
        .unwrap_or(false);
    let next_step_name = obj
        .get("next_step_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let next_step_description = obj
        .get("next_step_description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();

    if continue_execution && (next_step_name.is_empty() || next_step_description.is_empty()) {
        return Err(anyhow!(
            "completion evaluator 判定需要继续执行，但未提供 next_step_name/next_step_description"
        ));
    }

    Ok(CompletionSignal {
        result,
        continue_execution,
        next_step_name,
        next_step_description,
    })
}

fn truncate_summary_text(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }
    let mut out = raw.chars().take(max_chars).collect::<String>();
    out.push_str("...(truncated)");
    out
}

fn runtime_message(role: MessageRole, content: impl Into<String>) -> Message {
    Message {
        id: scru128::new().to_string(),
        role,
        content: content.into(),
        reasoning_content: String::new(),
        created_at: now_text(),
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
13. 若配置缺失，直接执行目标业务命令并根据错误回传定位问题，不要先做环境枚举。
14. 当前步骤支持多轮执行：若本轮尚未完成，请继续调用工具；最终必须调用 `mark_step_completed` 结束该步骤。
15. 当工具返回成功输出时，应先判断是否已满足用户请求；若仅是中间结果，调用 `mark_step_completed(continue_execution=true)` 并提供下一步。

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

当前步骤：
- name: {step_name}
- description: {step_desc}"#,
        plan_name = plan_name,
        previous_plan_result_text = previous_plan_result_text,
        skill_context = skill_context,
        step_name = step.name,
        step_desc = step.description
    )
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
            description: "读取文件内容，支持按行范围读取".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "start_line": { "type": "integer", "description": "起始行（从 1 开始，默认 1）", "minimum": 1 },
                    "max_lines": { "type": "integer", "description": "最大读取行数（默认 200，最大 2000）", "minimum": 1, "maximum": 2000 }
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
            description: "写入文件内容（支持覆盖或追加）".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "content": { "type": "string", "description": "要写入的内容" },
                    "append": { "type": "boolean", "description": "是否追加写入，默认 false（覆盖）" }
                },
                "required": ["path", "content"]
            }),
        },
        FunctionToolSpec {
            name: "replace_in_file".to_string(),
            description: "在文件中将旧文本替换为新文本，默认仅允许单点替换".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径" },
                    "old": { "type": "string", "description": "待替换的旧文本" },
                    "new": { "type": "string", "description": "替换后的新文本" },
                    "replace_all": { "type": "boolean", "description": "是否替换全部命中，默认 false" },
                    "expected_count": { "type": "integer", "description": "预期命中数量（可选）", "minimum": 1 }
                },
                "required": ["path", "old", "new"]
            }),
        },
        FunctionToolSpec {
            name: "run_command".to_string(),
            description: "执行受控命令，支持 cwd。shell 脚本建议使用 run_shell".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "命令名，例如 ls/cat/echo/pwd/rg/cargo/git/node/npx/npm/yarn/pnpm/ts-node" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "命令参数列表"
                    },
                    "cwd": { "type": "string", "description": "命令工作目录（可选，默认当前工作目录）" }
                },
                "required": ["cmd"]
            }),
        },
        FunctionToolSpec {
            name: "run_shell".to_string(),
            description: "执行 shell 脚本，自动派生 bash/sh/powershell 参数".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": "shell 脚本文本" },
                    "shell": { "type": "string", "description": "shell 类型：auto/bash/sh/powershell/pwsh，默认 auto" },
                    "cwd": { "type": "string", "description": "命令工作目录（可选）" }
                },
                "required": ["script"]
            }),
        },
        FunctionToolSpec {
            name: "apply_patch".to_string(),
            description: "对文件应用补丁文本，仅支持 unified diff（---/+++/@@）".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string", "description": "补丁内容文本（unified diff）" },
                    "verify": { "type": "boolean", "description": "是否仅校验不落盘（dry-run）" },
                    "workdir": { "type": "string", "description": "补丁工作目录（可选，默认当前工作目录）" }
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
                    "result": { "type": "string", "description": "本步骤完成结果摘要" },
                    "continue_execution": {
                        "type": "boolean",
                        "description": "当前 plan 是否需要继续执行后续动态步骤。true 表示需要追加下一步。"
                    },
                    "next_step_name": {
                        "type": "string",
                        "description": "当 continue_execution=true 时，下一步名称（必填）。"
                    },
                    "next_step_description": {
                        "type": "string",
                        "description": "当 continue_execution=true 时，下一步描述（必填）。"
                    }
                },
                "required": ["continue_execution"]
            }),
        },
    ]
}

#[derive(Debug, Clone)]
struct CompletionSignal {
    result: String,
    continue_execution: bool,
    next_step_name: String,
    next_step_description: String,
}

fn parse_completion_signal(call: &ModelFunctionCall) -> Result<CompletionSignal> {
    if call.name != "mark_step_completed" {
        return Err(anyhow!(
            "内部错误：parse_completion_signal 收到非 mark_step_completed 调用"
        ));
    }
    let result = call
        .arguments
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let continue_execution = call
        .arguments
        .get("continue_execution")
        .and_then(Value::as_bool)
        .or_else(|| {
            call.arguments
                .get("continue_execution")
                .and_then(Value::as_str)
                .and_then(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" | "on" => Some(true),
                    "false" | "0" | "no" | "off" => Some(false),
                    _ => None,
                })
        })
        .ok_or_else(|| anyhow!("mark_step_completed 缺少 continue_execution(bool) 参数"))?;
    let next_step_name = call
        .arguments
        .get("next_step_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let next_step_description = call
        .arguments
        .get("next_step_description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();

    Ok(CompletionSignal {
        result,
        continue_execution,
        next_step_name,
        next_step_description,
    })
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
            if let Some(start_line) = call
                .arguments
                .get("start_line")
                .and_then(number_or_string_to_text)
            {
                args.push(path.clone());
                args.push(start_line);
                if let Some(max_lines) = call
                    .arguments
                    .get("max_lines")
                    .and_then(number_or_string_to_text)
                {
                    args.push(max_lines);
                }
                return Ok(ToolCall {
                    name: ToolName::ReadFile,
                    args,
                });
            }
            args.push(path);
            if let Some(max_lines) = call
                .arguments
                .get("max_lines")
                .and_then(number_or_string_to_text)
            {
                args.push("1".to_string());
                args.push(max_lines);
            }
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
            if let Some(append) = call
                .arguments
                .get("append")
                .and_then(bool_or_string_to_text)
            {
                args.push(append);
            }
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
            if let Some(replace_all) = call
                .arguments
                .get("replace_all")
                .and_then(bool_or_string_to_text)
            {
                args.push(replace_all);
            }
            if let Some(expected_count) = call
                .arguments
                .get("expected_count")
                .and_then(number_or_string_to_text)
            {
                if args.len() == 3 {
                    args.push("false".to_string());
                }
                args.push(expected_count);
            }
            Ok(ToolCall {
                name: ToolName::ReplaceInFile,
                args,
            })
        }
        "run_command" => {
            let raw_cmd = call
                .arguments
                .get("cmd")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if let Some(mut parts) = split_command_parts(&raw_cmd)
                && !parts.is_empty()
            {
                args.push(parts.remove(0));
                args.extend(parts);
            }
            if args.is_empty() {
                args.push(raw_cmd);
            }
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
            if let Some(cwd) = call
                .arguments
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                args.push(format!("{INTERNAL_CWD_PREFIX}{cwd}"));
            }
            Ok(ToolCall {
                name: ToolName::RunCommand,
                args,
            })
        }
        "run_shell" => {
            let script = call
                .arguments
                .get("script")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let shell = call
                .arguments
                .get("shell")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("auto")
                .to_string();
            args.push(INTERNAL_SHELL_CMD.to_string());
            args.push(script);
            args.push(shell);
            if let Some(cwd) = call
                .arguments
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                args.push(format!("{INTERNAL_CWD_PREFIX}{cwd}"));
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
            args.push(INTERNAL_SHELL_CMD.to_string());
            args.push(script);
            args.push("bash".to_string());
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
            if let Some(verify) = call
                .arguments
                .get("verify")
                .and_then(bool_or_string_to_text)
            {
                args.push(verify);
            }
            if let Some(workdir) = call
                .arguments
                .get("workdir")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                if args.len() == 1 {
                    args.push("false".to_string());
                }
                args.push(workdir.to_string());
            }
            Ok(ToolCall {
                name: ToolName::ApplyPatch,
                args,
            })
        }
        _ => Err(anyhow!("未知函数调用：{}", call.name)),
    }?;
    Ok(tool_call)
}

fn number_or_string_to_text(value: &serde_json::Value) -> Option<String> {
    value
        .as_u64()
        .map(|v| v.to_string())
        .or_else(|| value.as_i64().map(|v| v.to_string()))
        .or_else(|| value.as_str().map(ToString::to_string))
}

fn bool_or_string_to_text(value: &serde_json::Value) -> Option<String> {
    value
        .as_bool()
        .map(|v| v.to_string())
        .or_else(|| value.as_str().map(ToString::to_string))
}

fn split_command_parts(raw: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if !in_single => {
                escaped = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped || in_single || in_double {
        return None;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() { None } else { Some(out) }
}
