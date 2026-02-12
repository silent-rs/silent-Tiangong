use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolName {
    ReadFile,
    ListDir,
    RunCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: ToolName,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub trait ToolExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult>;
}

#[derive(Debug, Default)]
pub struct PlaceholderToolExecutor;

impl ToolExecutor for PlaceholderToolExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        Ok(ToolResult {
            ok: true,
            summary: format!("Phase 1 占位执行：{:?}", call.name),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
        })
    }
}
