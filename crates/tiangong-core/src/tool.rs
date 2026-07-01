//! 工具结果类型。
//!
//! LocalToolExecutor 及其内置工具（web_fetch / run_command）已全部迁出为进程内插件：
//! - list_dir / read_file / write_file 等 8 个 → tiangong-plugin-fs
//! - web_fetch → tiangong-plugin-fetch（CLI/Server）/ browser 插件（GUI）
//! - run_command / run_shell → tiangong-plugin-command（CLI/Server）/ terminal 插件（GUI）
//!
//! core 不再直接执行任何工具，仅保留 ToolResult 供插件 handler 返回。
//! background_task（spawn_task 后台任务）仍由 core 的 runtime.rs 直接处理，
//! common（路径沙箱/命令白名单）暴露给插件 crate 复用。

pub mod background_task;
pub mod common;
pub use common::{session_workspace_root, set_session_cwd};

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionRecord {
    pub tool_name: String,
    pub args: Vec<String>,
    pub duration_ms: u64,
    pub ok: bool,
    pub exit_code: i32,
    pub summary: String,
}
