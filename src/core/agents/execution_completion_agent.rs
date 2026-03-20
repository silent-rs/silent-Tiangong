use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::core::execution::SuccessfulBusinessResult;
use crate::core::model::{
    ModelClient, ModelFunctionCall, ModelRequest, ModelStreamChunk, SingleProviderClient,
    TokenUsage,
};
use crate::core::planner::PlanStep;
use crate::core::session::{Message, Session};
#[allow(clippy::too_many_arguments)]
pub(crate) fn infer_completion_signal_with_llm(
    client: &SingleProviderClient,
    session: &Session,
    context: &[Message],
    user_input: &str,
    plan_name: &str,
    step: &PlanStep,
    successful_result: &SuccessfulBusinessResult,
    round_feedback: &[String],
    on_chunk: &mut dyn FnMut(&ModelStreamChunk),
    accumulated_usage: &mut TokenUsage,
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
    let response = client.complete_stream(&request, on_chunk)?;
    accumulated_usage.accumulate(&response.usage);
    let payload = parse_json_object_from_text(&response.text)?;
    parse_completion_signal_from_json(&payload, successful_result.summary.as_str())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn review_completion_signal_with_llm(
    client: &SingleProviderClient,
    session: &Session,
    context: &[Message],
    user_input: &str,
    plan_name: &str,
    step: &PlanStep,
    successful_result: &SuccessfulBusinessResult,
    round_feedback: &[String],
    proposed_signal: &CompletionSignal,
    on_chunk: &mut dyn FnMut(&ModelStreamChunk),
    accumulated_usage: &mut TokenUsage,
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
    let response = client.complete_stream(&request, on_chunk)?;
    accumulated_usage.accumulate(&response.usage);
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

pub(crate) fn parse_completion_signal_from_json(
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

#[derive(Debug, Clone)]
pub(crate) struct CompletionSignal {
    pub(crate) result: String,
    pub(crate) continue_execution: bool,
    pub(crate) next_step_name: String,
    pub(crate) next_step_description: String,
}

pub(crate) fn parse_completion_signal(call: &ModelFunctionCall) -> Result<CompletionSignal> {
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
