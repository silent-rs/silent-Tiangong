use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::model::ModelFunctionCall;

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
