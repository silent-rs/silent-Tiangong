use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent_config::AgentConfig;

pub mod background_task;
mod common;
mod current_time;
pub use common::set_session_cwd;
mod list_dir;
mod read_file;
mod replace_in_file;
mod run_command;
mod scheduler;
mod search_code;
mod tree_dir;
mod web_fetch;
mod write_file;

#[cfg(feature = "diffy")]
mod apply_patch;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolName {
    ReadFile,
    ListDir,
    TreeDir,
    RunCommand,
    SearchCode,
    CurrentTime,
    Scheduler,
    WebFetch,
    WriteFile,
    ReplaceInFile,
    #[cfg(feature = "diffy")]
    ApplyPatch,
}

impl ToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::ListDir => "list_dir",
            Self::TreeDir => "tree_dir",
            Self::RunCommand => "run_command",
            Self::SearchCode => "search_code",
            Self::CurrentTime => "current_time",
            Self::Scheduler => "scheduler",
            Self::WebFetch => "web_fetch",
            Self::WriteFile => "write_file",
            Self::ReplaceInFile => "replace_in_file",
            #[cfg(feature = "diffy")]
            Self::ApplyPatch => "apply_patch",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: ToolName,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionRecord {
    pub tool_name: String,
    pub args: Vec<String>,
    pub duration_ms: u64,
    pub ok: bool,
    pub exit_code: i32,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    #[serde(default)]
    pub execution: Option<ToolExecutionRecord>,
}

pub trait ToolExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult>;
}

#[derive(Debug, Clone, Default)]
pub struct LocalToolExecutor {
    runtime_env: BTreeMap<String, String>,
    /// 共享信任模式引用，FullTrust 时跳过路径越界和命令白名单检查
    shared_trust_mode: Option<std::sync::Arc<std::sync::RwLock<crate::permission::TrustMode>>>,
}

impl ToolExecutor for LocalToolExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let started = Instant::now();
        let result = match call.name {
            ToolName::ReadFile => self.read_file(call),
            ToolName::ListDir => self.list_dir(call),
            ToolName::TreeDir => self.tree_dir(call),
            ToolName::RunCommand => self.run_command(call),
            ToolName::SearchCode => self.search_code(call),
            ToolName::CurrentTime => self.current_time(call),
            ToolName::Scheduler => self.scheduler(call),
            ToolName::WebFetch => self.web_fetch(call),
            ToolName::WriteFile => self.write_file(call),
            ToolName::ReplaceInFile => self.replace_in_file(call),
            #[cfg(feature = "diffy")]
            ToolName::ApplyPatch => self.apply_patch(call),
        };
        let duration_ms = common::elapsed_ms_u64(started.elapsed().as_millis());

        Ok(match result {
            Ok(mut ok) => {
                ok.execution = Some(ToolExecutionRecord {
                    tool_name: call.name.as_str().to_string(),
                    args: call.args.clone(),
                    duration_ms,
                    ok: ok.ok,
                    exit_code: ok.exit_code,
                    summary: ok.summary.clone(),
                });
                ok
            }
            Err(err) => {
                let summary = format!("工具执行失败：{err}");
                ToolResult {
                    ok: false,
                    summary: summary.clone(),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: Some(ToolExecutionRecord {
                        tool_name: call.name.as_str().to_string(),
                        args: call.args.clone(),
                        duration_ms,
                        ok: false,
                        exit_code: 1,
                        summary,
                    }),
                }
            }
        })
    }
}

impl LocalToolExecutor {
    pub fn from_agent_config(agent_config: &AgentConfig) -> Self {
        Self {
            runtime_env: run_command::collect_runtime_env(agent_config),
            shared_trust_mode: None,
        }
    }

    /// 设置共享信任模式引用
    pub fn with_shared_trust_mode(
        mut self,
        shared: std::sync::Arc<std::sync::RwLock<crate::permission::TrustMode>>,
    ) -> Self {
        self.shared_trust_mode = Some(shared);
        self
    }

    /// 当前是否处于完全信任模式
    pub(super) fn is_full_trust(&self) -> bool {
        self.shared_trust_mode
            .as_ref()
            .and_then(|s| s.read().ok())
            .map(|g| *g == crate::permission::TrustMode::FullTrust)
            .unwrap_or(false)
    }

    pub(super) fn runtime_env(&self) -> &BTreeMap<String, String> {
        &self.runtime_env
    }
}
