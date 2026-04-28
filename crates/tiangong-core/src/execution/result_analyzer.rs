use serde_json::Value;

use crate::execution::ExecutionLlmOutput;
use crate::model::{ModelFunctionResponse, TokenUsage};
use crate::tool::ToolResult;

const SUCCESS_RESULT_PREVIEW_MAX_CHARS: usize = 500;

#[derive(Debug, Clone)]
pub(crate) struct SuccessfulBusinessResult {
    pub(crate) summary: String,
}

pub(crate) fn build_tool_failure_error(result: &ToolResult) -> String {
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

pub(crate) fn collect_llm_output(
    round: usize,
    response: &ModelFunctionResponse,
    output_contents: &mut Vec<String>,
    output_reasonings: &mut Vec<String>,
    output_tool_calls: &mut Vec<String>,
    accumulated_usage: &mut TokenUsage,
) {
    accumulated_usage.accumulate(&response.usage);
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

pub(crate) fn build_aggregated_llm_output(
    output_contents: Vec<String>,
    output_reasonings: Vec<String>,
    output_tool_calls: Vec<String>,
    usage: TokenUsage,
) -> Option<ExecutionLlmOutput> {
    if output_contents.is_empty()
        && output_reasonings.is_empty()
        && output_tool_calls.is_empty()
        && usage.total_tokens == 0
    {
        return None;
    }
    Some(ExecutionLlmOutput {
        content: output_contents.join("\n"),
        reasoning_content: output_reasonings.join("\n"),
        tool_calls: output_tool_calls,
        usage,
    })
}

pub(crate) fn summarize_round_tool_feedback(result: &ToolResult) -> String {
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

pub(crate) fn extract_successful_business_result(
    result: &ToolResult,
) -> Option<SuccessfulBusinessResult> {
    if !result.ok {
        return None;
    }
    let tool_name = result
        .execution
        .as_ref()
        .map(|record| record.tool_name.as_str())
        .unwrap_or_default();
    let stdout = result.stdout.trim();
    if stdout.is_empty() {
        return None;
    }

    if tool_name.starts_with("mcp::") {
        if let Ok(value) = serde_json::from_str::<Value>(stdout) {
            let compact = serde_json::to_string(&value).unwrap_or_else(|_| {
                truncate_summary_text(stdout, SUCCESS_RESULT_PREVIEW_MAX_CHARS)
            });
            return Some(SuccessfulBusinessResult {
                summary: format!(
                    "MCP 调用返回结构化结果: {}",
                    truncate_summary_text(&compact, SUCCESS_RESULT_PREVIEW_MAX_CHARS)
                ),
            });
        }
        return Some(SuccessfulBusinessResult {
            summary: format!(
                "MCP 调用返回文本结果: {}",
                truncate_summary_text(stdout, SUCCESS_RESULT_PREVIEW_MAX_CHARS)
            ),
        });
    }

    if tool_name != "run_command" {
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
        });
    }

    let normalized = stdout.replace(' ', "");
    if normalized.contains("\"success\":true") {
        return Some(SuccessfulBusinessResult {
            summary: format!(
                "run_command 输出包含 success=true: {}",
                truncate_summary_text(stdout, SUCCESS_RESULT_PREVIEW_MAX_CHARS)
            ),
        });
    }
    None
}

pub(crate) fn truncate_summary_text(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }
    let mut out = raw.chars().take(max_chars).collect::<String>();
    out.push_str("...(truncated)");
    out
}
