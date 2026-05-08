use anyhow::{Result, anyhow};

use crate::agent_config::McpConfig;
use crate::execution::{
    DynamicPlanStep, ExecutionStepReport, ExecutionStepResult, build_aggregated_llm_output,
    build_tool_failure_error, collect_llm_output, extract_successful_business_result,
    runtime_message, summarize_round_tool_feedback,
};
use crate::model::{ModelClient, ModelRequest, ModelStreamChunk, SingleProviderClient, TokenUsage};
use crate::planner::{PlanStep, PlanStepStatus, TaskPlan};
use crate::session::{Message, MessageRole, Session, now_text};
use crate::tool::{LocalToolExecutor, ToolExecutor, ToolResult};

use crate::agents::execution_completion_agent::parse_completion_signal;
use crate::agents::execution_mcp_agent::{
    build_internal_tool_error_result, execute_mcp_tool_call, execute_mcp_tool_call_with_args,
    execution_function_tools, normalize_mcp_call_arguments, resolve_mcp_tool_call_from_run_command,
    resolve_unique_mcp_target_by_raw_name,
};
use crate::agents::execution_prompt_agent::{
    build_step_execution_prompt, build_step_followup_prompt,
};
use crate::agents::execution_tool_agent::build_tool_call_from_function;

const MAX_EXECUTION_AGENT_ROUNDS: usize = 6;
#[allow(clippy::too_many_arguments)]
pub fn execute_single_plan_step_with_execution_agent(
    client: &SingleProviderClient,
    tool_executor: &LocalToolExecutor,
    mcp_config: &McpConfig,
    session: &Session,
    user_input: &str,
    context: &[Message],
    plan: &TaskPlan,
    plan_name: &str,
    step: &PlanStep,
    previous_plan_summaries: &[String],
    tool_results: &mut Vec<ToolResult>,
    on_chunk: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<ExecutionStepResult> {
    let (function_tools, mcp_function_targets) = execution_function_tools(mcp_config);
    let step_prompt = build_step_execution_prompt(
        user_input,
        plan,
        plan_name,
        step,
        previous_plan_summaries,
        &mcp_function_targets,
    );
    let mut loop_context = context.to_vec();
    let mut output_contents = Vec::new();
    let mut output_reasonings = Vec::new();
    let mut output_tool_calls = Vec::new();
    let mut executed_tools = Vec::new();
    let mut step_usage = TokenUsage::default();

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
            assembled_system_prompt: None,
            thinking: Some(crate::model::ModelThinkingConfig {
                budget_tokens: 4096,
            }),
            include_media: false,
        };
        let response =
            client.complete_with_functions_stream(&request, &function_tools, on_chunk)?;
        let _ = response.usage.total_tokens;
        collect_llm_output(
            round + 1,
            &response,
            &mut output_contents,
            &mut output_reasonings,
            &mut output_tool_calls,
            &mut step_usage,
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
        let mut successful_result = None;
        let mut ignored_failed_tool = false;
        let mut round_feedback = Vec::new();
        let mut blocking_errors = Vec::new();
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
            let result = match (|| -> Result<ToolResult> {
                if let Some(target) = mcp_function_targets.get(&tool_call.name) {
                    return execute_mcp_tool_call(tool_call, target, mcp_config);
                }
                if let Some((target, args)) = resolve_mcp_tool_call_from_run_command(
                    tool_call,
                    &mcp_function_targets,
                    mcp_config,
                ) {
                    return execute_mcp_tool_call_with_args(&target, args, mcp_config);
                }
                match build_tool_call_from_function(tool_call) {
                    Ok(call) => tool_executor.execute(&call),
                    Err(err) => {
                        if let Some(target) =
                            resolve_unique_mcp_target_by_raw_name(&tool_call.name, mcp_config)
                        {
                            execute_mcp_tool_call_with_args(
                                &target,
                                normalize_mcp_call_arguments(tool_call),
                                mcp_config,
                            )
                        } else {
                            Err(err)
                        }
                    }
                }
            })() {
                Ok(result) => result,
                Err(err) => build_internal_tool_error_result(&tool_call.name, &err.to_string()),
            };
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
                blocking_errors.push(build_tool_failure_error(&result));
                continue;
            }
        }

        if !blocking_errors.is_empty() {
            if round + 1 >= MAX_EXECUTION_AGENT_ROUNDS {
                return Err(anyhow!(
                    "执行智能体在 {} 轮内持续工具失败：step={} {}，last_error={}",
                    MAX_EXECUTION_AGENT_ROUNDS,
                    step.name,
                    step.description,
                    blocking_errors
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string())
                ));
            }
            loop_context.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::Tool,
                content: format!(
                    "执行反馈：上一轮工具调用失败，请修正后继续。\n错误列表：\n{}\n要求：必须优先直接调用函数工具（尤其 MCP 工具），不要将工具名当 shell 命令。",
                    blocking_errors.join("\n")
                ),
                reasoning_content: String::new(),
                reasoning_signature: None,
                worker_id: None,
                media: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_name: Some("execution_feedback".to_string()),
                tool_result_is_error: false,
                compact: false,
                created_at: now_text(),
            });
            continue;
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

            let llm_output = build_aggregated_llm_output(
                output_contents,
                output_reasonings,
                output_tool_calls,
                step_usage.clone(),
            );

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
            if round + 1 >= MAX_EXECUTION_AGENT_ROUNDS {
                return Err(anyhow!(
                    "执行智能体已获得成功工具结果但未提交 mark_step_completed：step={} {}，result={}",
                    step.name,
                    step.description,
                    successful_result.summary
                ));
            }
            let mut feedback = format!(
                "执行反馈：上一轮工具已经返回明确成功结果，但尚未调用 mark_step_completed。\n\
                 成功结果：{}\n\
                 要求：不要重复执行已经成功的工具。若结果已满足当前步骤和用户目标，直接调用 mark_step_completed(continue_execution=false)；若只是中间结果，调用 mark_step_completed(continue_execution=true) 并给出下一步名称和描述。",
                successful_result.summary
            );
            if ignored_failed_tool {
                feedback.push_str("\n注意：成功结果之后的额外失败调用已忽略，请基于成功结果收敛。");
            }
            loop_context.push(runtime_message(MessageRole::Tool, feedback));
            continue;
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
            MessageRole::Tool,
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
