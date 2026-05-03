use anyhow::Result;
use chrono::{Local, SecondsFormat};
use serde_json::json;

use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn current_time(&self, _call: &ToolCall) -> Result<ToolResult> {
        let now = Local::now();
        let output = json!({
            "local_time": now.naive_local().to_string(),
            "rfc3339": now.to_rfc3339_opts(SecondsFormat::Secs, false),
            "unix_timestamp": now.timestamp(),
            "timezone_offset": now.offset().to_string(),
        });
        Ok(ToolResult {
            ok: true,
            summary: format!("当前本地时间：{}", now.naive_local()),
            stdout: serde_json::to_string_pretty(&output)?,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}
