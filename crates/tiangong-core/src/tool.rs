use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent_config::AgentConfig;

pub mod background_task;
pub mod common;
pub use common::{session_workspace_root, set_session_cwd};
mod run_command;
mod web_fetch;

/// LocalToolExecutor 内置工具名。
///
/// 仅保留 web_fetch / run_command——其余基础文件工具（list_dir / read_file /
/// write_file / tree_dir / search_code / current_time / replace_in_file /
/// apply_patch）已迁出至 `tiangong-plugin-fs` 进程内插件，core 不再直接感知。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolName {
    RunCommand,
    WebFetch,
}

impl ToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RunCommand => "run_command",
            Self::WebFetch => "web_fetch",
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
    fn execute(&self, call: &ToolCall, session_id: &str) -> Result<ToolResult>;
}

#[derive(Clone, Default)]
pub struct LocalToolExecutor {
    runtime_env: BTreeMap<String, String>,
    /// 共享信任模式引用，FullTrust 时跳过路径越界和命令白名单检查
    shared_trust_mode: Option<std::sync::Arc<std::sync::RwLock<crate::permission::TrustMode>>>,
    /// 终端会话能力（GUI 模式下由 Tauri Plugin 提供，校验通过后 run_command 走 PTY）
    terminal_provider: Option<std::sync::Arc<dyn crate::terminal_trait::TerminalProvider>>,
}

impl ToolExecutor for LocalToolExecutor {
    fn execute(&self, call: &ToolCall, session_id: &str) -> Result<ToolResult> {
        let started = Instant::now();
        let result = match call.name {
            ToolName::RunCommand => self.run_command(call, session_id),
            ToolName::WebFetch => self.web_fetch(call),
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
            terminal_provider: None,
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

    /// 设置终端会话能力（校验通过后可通过 PTY 执行命令）
    pub fn with_terminal_provider(
        mut self,
        provider: std::sync::Arc<dyn crate::terminal_trait::TerminalProvider>,
    ) -> Self {
        self.terminal_provider = Some(provider);
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

    pub(super) fn terminal_provider(
        &self,
    ) -> &Option<std::sync::Arc<dyn crate::terminal_trait::TerminalProvider>> {
        &self.terminal_provider
    }
}
