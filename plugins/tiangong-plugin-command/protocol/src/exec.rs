//! 命令执行链路操作（run_command / run_shell + set_workspace）。

use serde::{Deserialize, Serialize};

use crate::{Ack, CommandAccessContext, CommandOperation};

pub const RUN_COMMAND_OPERATION: &str = "command.run_command";
pub const RUN_SHELL_OPERATION: &str = "command.run_shell";
pub const SET_WORKSPACE_OPERATION: &str = "command.set_workspace";

/// 命令执行响应：保留与 core `ToolResult` 同构字段，便于 sidecar 直接构造。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecResponse {
    pub ok: bool,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// 请求级全权执行声明（RFC 0017 S4 升级审批闭环 v1）。
///
/// Agent 在预分类拒绝或沙箱拦截后，先经 `request_user`（kind: approval）
/// 获得用户批准，再携带本声明以全权重跑。`approval_note` 记录批准依据，
/// 全程审计留痕；宿主验证审批结果的正式闭环见 RFC §14 开放问题。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EscalatedRequest {
    /// 用户批准的依据描述（如批准弹窗中的命令与影响说明）。
    #[serde(default)]
    pub approval_note: String,
    /// 宿主签发的一次性升级审批票据；宿主转发层核验（无效即剥离声明）。
    #[serde(default)]
    pub token: String,
}

/// `run_command` 工具请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunCommandRequest {
    /// 命令名（可含参数，由 sidecar 拆分）。
    pub cmd: String,
    /// 命令参数列表（追加到 cmd 拆分结果后）。
    #[serde(default)]
    pub args: Vec<String>,
    /// 工作目录（可选，默认会话工作目录）。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 超时时间（秒），0 或不填表示不限时。
    #[serde(default)]
    pub timeout_secs: u64,
    #[serde(flatten)]
    pub access: CommandAccessContext,
    /// 全权执行声明（S4）：携带即跳过沙箱，审计留痕。
    #[serde(default)]
    pub escalated: Option<EscalatedRequest>,
}
pub struct RunCommand;
impl CommandOperation for RunCommand {
    const NAME: &'static str = RUN_COMMAND_OPERATION;
    type Request = RunCommandRequest;
    type Response = ExecResponse;
}

/// `run_shell` 工具请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunShellRequest {
    /// shell 脚本文本。
    pub script: String,
    /// shell 类型：auto/bash/sh/powershell/pwsh，默认 auto。
    #[serde(default)]
    pub shell: String,
    /// 工作目录（可选）。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 超时时间（秒），0 或不填表示不限时。
    #[serde(default)]
    pub timeout_secs: u64,
    #[serde(flatten)]
    pub access: CommandAccessContext,
    /// 全权执行声明（S4）：携带即跳过沙箱，审计留痕。
    #[serde(default)]
    pub escalated: Option<EscalatedRequest>,
}

pub struct RunShell;
impl CommandOperation for RunShell {
    const NAME: &'static str = RUN_SHELL_OPERATION;
    type Request = RunShellRequest;
    type Response = ExecResponse;
}

/// `set_workspace` 钩子请求：通知 sidecar 工作区与信任模式变更。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetWorkspaceRequest {
    /// 新工作目录；None 表示清除。
    #[serde(default)]
    pub workspace: Option<String>,
    /// 是否完全信任模式。
    #[serde(default)]
    pub full_trust: bool,
    /// 用户自定义允许命令列表。
    #[serde(default)]
    pub allowed_commands: Vec<String>,
}
pub struct SetWorkspace;
impl CommandOperation for SetWorkspace {
    const NAME: &'static str = SET_WORKSPACE_OPERATION;
    type Request = SetWorkspaceRequest;
    type Response = Ack;
}
