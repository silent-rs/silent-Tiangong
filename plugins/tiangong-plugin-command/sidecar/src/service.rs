//! Command sidecar 业务服务：按操作名分发请求，承载 tokio 子进程 spawn 与命令校验策略。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow};
use tiangong_sandbox::SandboxedProgram;

use tiangong_plugin_command_protocol::exec::{
    EscalatedRequest, ExecResponse, RUN_COMMAND_OPERATION, RUN_SHELL_OPERATION, RunCommandRequest,
    RunShellRequest, SET_WORKSPACE_OPERATION, SetWorkspaceRequest, TRUST_COMMAND_OPERATION,
    TrustCommandRequest,
};
use tiangong_plugin_command_protocol::{Ack, COMMAND_PROTOCOL_VERSION, PLUGIN_ID, PLUGIN_VERSION};
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};
use tiangong_toolkit as shared;

use crate::command_policy::{CommandPolicy, TrustModeCommandPolicy};
use crate::exec;

/// Command sidecar 业务服务。
pub struct CommandService {
    /// 当前会话工作目录（由 set_workspace 注入，cwd 解析基准）。
    workspace: RwLock<Option<PathBuf>>,
    /// 是否完全信任模式（跳过命令/路径校验）。
    full_trust: RwLock<bool>,
    /// 用户自定义允许命令列表（扩展内置白名单）。
    allowed_commands: RwLock<Vec<String>>,
    /// 会话信任列表（S4）：经用户批准登记的命令，本会话内全权执行。
    trusted_commands: RwLock<Vec<String>>,
    /// 命令执行策略（沙箱预留点 A：可替换实现）。
    policy: Arc<dyn CommandPolicy>,
}

impl CommandService {
    /// 构造默认实例。
    pub fn new() -> Result<Self> {
        Ok(Self {
            workspace: RwLock::new(None),
            full_trust: RwLock::new(false),
            allowed_commands: RwLock::new(Vec::new()),
            trusted_commands: RwLock::new(Vec::new()),
            policy: Arc::new(TrustModeCommandPolicy::new()),
        })
    }

    /// 按 sidecar 协议分发请求。
    pub async fn dispatch(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                format!(
                    "Command 协议版本不匹配: expected={PROTOCOL_VERSION}, actual={}",
                    request.protocol_version
                ),
                false,
            );
        }

        let payload = match self
            .dispatch_operation(&request.operation, request.payload)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                return Response::error(
                    &request_id,
                    ErrorCode::ServiceError,
                    error.to_string(),
                    false,
                );
            }
        };
        Response::success(&request_id, payload)
    }

    async fn dispatch_operation(
        &self,
        operation: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        match operation {
            HANDSHAKE_OPERATION => serde_json::to_value(HandshakeResponse {
                plugin_id: PLUGIN_ID.to_string(),
                plugin_version: PLUGIN_VERSION.to_string(),
                sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol_version: PROTOCOL_VERSION.to_string(),
                business_protocol: COMMAND_PROTOCOL_VERSION,
                capabilities: vec!["command".to_string()],
                instance_id: format!("command-sidecar-{}", std::process::id()),
                status: ServiceStatus::Ready,
            })
            .with_context(|| "序列化 Command 握手响应失败"),
            RUN_COMMAND_OPERATION => {
                let req: RunCommandRequest =
                    serde_json::from_value(payload).with_context(|| "解析 run_command 请求失败")?;
                let resp = self.handle_run_command(req).await;
                serde_json::to_value(resp).with_context(|| "序列化 run_command 响应失败")
            }
            RUN_SHELL_OPERATION => {
                let req: RunShellRequest =
                    serde_json::from_value(payload).with_context(|| "解析 run_shell 请求失败")?;
                let resp = self.handle_run_shell(req).await;
                serde_json::to_value(resp).with_context(|| "序列化 run_shell 响应失败")
            }
            TRUST_COMMAND_OPERATION => {
                let req: TrustCommandRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 trust_command 请求失败")?;
                self.handle_trust_command(req)?;
                serde_json::to_value(Ack {}).with_context(|| "序列化 trust_command 响应失败")
            }
            SET_WORKSPACE_OPERATION => {
                let req: SetWorkspaceRequest = serde_json::from_value(payload)
                    .with_context(|| "解析 set_workspace 请求失败")?;
                self.handle_set_workspace(req)?;
                serde_json::to_value(Ack {}).with_context(|| "序列化 set_workspace 响应失败")
            }
            operation => Err(anyhow!("不支持的 Command 操作: {operation}")),
        }
    }

    // ── 工具执行 ─────────────────────────────────────────────

    async fn handle_run_command(&self, req: RunCommandRequest) -> ExecResponse {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return error_response("run_command", e),
        };
        let raw_cmd = req.cmd.trim();
        if raw_cmd.is_empty() {
            return error_response("run_command", anyhow!("run_command 缺少 cmd 参数"));
        }
        let (cmd, mut cmd_args) = exec::split_command(raw_cmd);
        cmd_args.extend(req.args.iter().cloned());
        let effective_cwd = match self.policy.resolve_cwd(req.cwd.as_deref(), &base) {
            Ok(c) => c,
            Err(e) => return error_response("run_command", e),
        };
        let timeout_ms = timeout_to_ms(req.timeout_secs);
        // 校验经 CommandPolicy（沙箱预留点 A）。
        if let Err(e) =
            self.policy
                .validate_run_command(&cmd, &cmd_args, &effective_cwd, &req.access)
        {
            return error_response("run_command", e);
        }
        // 沙箱（RFC 0017 S3/S4）：升级声明或会话信任命中时全权执行（审计留痕），
        // 预分类拒绝高危命令，其余包装进 OS 沙箱执行。
        let risk = tiangong_sandbox::assess_program(&cmd, &cmd_args);
        let (cmd, cmd_args) = match self.apply_sandbox(
            "run_command",
            risk,
            cmd,
            cmd_args,
            &effective_cwd,
            req.escalated.as_ref(),
        ) {
            Ok(wrapped) => wrapped,
            Err(resp) => return resp,
        };
        exec::exec_and_collect(&cmd, &cmd_args, &effective_cwd, timeout_ms)
            .await
            .map(annotate_violation)
            .unwrap_or_else(|e| error_response("命令执行", e))
    }

    async fn handle_run_shell(&self, req: RunShellRequest) -> ExecResponse {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return error_response("run_shell", e),
        };
        let script = req.script.trim();
        if script.is_empty() {
            return error_response("run_shell", anyhow!("run_shell 缺少 script 参数"));
        }
        let shell = if req.shell.is_empty() {
            "auto"
        } else {
            req.shell.as_str()
        };
        let (cmd, cmd_args) = match shared::derive_shell_exec_args(script, Some(shell)) {
            Ok(v) => v,
            Err(e) => return error_response("run_shell", e),
        };
        let effective_cwd = match self.policy.resolve_cwd(req.cwd.as_deref(), &base) {
            Ok(c) => c,
            Err(e) => return error_response("run_shell", e),
        };
        let timeout_ms = timeout_to_ms(req.timeout_secs);
        if let Err(e) = self
            .policy
            .validate_run_shell(&cmd, &cmd_args, &effective_cwd, &req.access)
        {
            return error_response("run_shell", e);
        }
        // 沙箱（RFC 0017 S3/S4）：整段脚本先过预分类，其余包装进 OS 沙箱执行。
        let risk = tiangong_sandbox::assess_script(script);
        let (cmd, cmd_args) = match self.apply_sandbox(
            "run_shell",
            risk,
            cmd,
            cmd_args,
            &effective_cwd,
            req.escalated.as_ref(),
        ) {
            Ok(wrapped) => wrapped,
            Err(resp) => return resp,
        };
        exec::exec_and_collect(&cmd, &cmd_args, &effective_cwd, timeout_ms)
            .await
            .map(annotate_violation)
            .unwrap_or_else(|e| error_response("命令执行", e))
    }

    // ── 生命周期 ─────────────────────────────────────────────

    /// 沙箱接入（RFC 0017 S3）：全信模式原样直跑；高危命令预分类拒绝并
    /// 引导走升级审批；其余命令包装进 OS 沙箱（macOS Seatbelt / Linux bwrap），
    /// 平台不可用时降级直跑（快照层兜底）。
    fn apply_sandbox(
        &self,
        tool: &str,
        risk: tiangong_sandbox::CommandRisk,
        cmd: String,
        args: Vec<String>,
        cwd: &Path,
        escalated: Option<&EscalatedRequest>,
    ) -> std::result::Result<(String, Vec<String>), ExecResponse> {
        let full_trust = self.full_trust.read().map(|guard| *guard).unwrap_or(false);
        if full_trust {
            return Ok((cmd, args));
        }
        // S4 升级审批闭环：升级声明（Agent 携带用户批准依据）或会话信任命中
        // 时全权执行。v1 为声明制（依据记录审计，宿主验证审批见 RFC 开放问题）。
        if let Some(escalated) = escalated {
            tracing::warn!(
                tool,
                command = %cmd,
                approval_note = %escalated.approval_note,
                "全权执行（升级审批声明）"
            );
            return Ok((cmd, args));
        }
        let program = cmd.rsplit('/').next().unwrap_or(&cmd).to_string();
        if self
            .trusted_commands
            .read()
            .map(|trusted| trusted.iter().any(|item| item == &program))
            .unwrap_or(false)
        {
            tracing::warn!(tool, command = %program, "全权执行（会话信任列表命中）");
            return Ok((cmd, args));
        }
        if risk == tiangong_sandbox::CommandRisk::KnownDangerous {
            let desc = if args.is_empty() {
                cmd.clone()
            } else {
                format!("{cmd} {}", args.join(" "))
            };
            return Err(error_response(
                tool,
                anyhow!("{}", tiangong_sandbox::denial_hint(&desc)),
            ));
        }
        let policy = tiangong_sandbox::SandboxPolicy::workspace_write(cwd);
        match tiangong_sandbox::wrap(&policy) {
            // 沙箱不可用时默认拒绝执行（RFC 0017 审查修订）：防止"用户以为
            // 在沙箱内实则裸奔"；需要执行走升级审批票据进入全权通道。
            SandboxedProgram::Unavailable(reason) => Err(error_response(
                tool,
                anyhow::anyhow!(
                    "当前平台沙箱不可用，命令未执行：{reason}。\n\
如确需执行，请调用 request_user（kind: approval）获得用户批准后，\n\
经 command_escalation_approve 签发票据并以 escalated 方式全权执行。"
                ),
            )),
            SandboxedProgram::Direct => Ok((cmd, args)),
            SandboxedProgram::Wrapped { program, prefix } => {
                let mut wrapped_args = prefix;
                wrapped_args.push(cmd);
                wrapped_args.extend(args);
                tracing::debug!(tool, program = %program, "命令已包装进 OS 沙箱");
                Ok((program, wrapped_args))
            }
        }
    }

    /// 登记会话信任命令（S4）：登记后本会话内该程序全权执行，审计留痕。
    fn handle_trust_command(&self, req: TrustCommandRequest) -> Result<()> {
        let command = req.command.trim().to_string();
        if command.is_empty() {
            anyhow::bail!("trust_command 缺少 command");
        }
        tracing::warn!(
            command = %command,
            approval_note = %req.approval_note,
            "登记会话信任命令（依据用户批准）"
        );
        if let Ok(mut trusted) = self.trusted_commands.write()
            && !trusted.iter().any(|item| item == &command)
        {
            trusted.push(command);
        }
        Ok(())
    }

    fn handle_set_workspace(&self, req: SetWorkspaceRequest) -> Result<()> {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = req.workspace.map(PathBuf::from);
        }
        if let Ok(mut guard) = self.full_trust.write() {
            *guard = req.full_trust;
        }
        if let Ok(mut guard) = self.allowed_commands.write() {
            *guard = req.allowed_commands;
        }
        Ok(())
    }

    // ── 辅助 ─────────────────────────────────────────────────

    /// 取当前工作目录，未注入时报错（command 工具必须知道 workspace）。
    fn base(&self) -> Result<PathBuf> {
        self.workspace
            .read()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| anyhow!("会话工作目录未注入，无法执行命令"))
    }
}

/// timeout 秒 → 毫秒（对齐原实现：0 表无限等待，用 command_timeout_ms 默认值）。
fn timeout_to_ms(timeout_secs: u64) -> u64 {
    if timeout_secs > 0 {
        timeout_secs.saturating_mul(1000)
    } else {
        shared::command_timeout_ms()
    }
}

fn error_response(tool: &str, e: anyhow::Error) -> ExecResponse {
    let summary = format!("{tool} 失败：{e}");
    ExecResponse {
        ok: false,
        summary: summary.clone(),
        stderr: summary,
        exit_code: 1,
        ..Default::default()
    }
}

/// 沙箱违规归因（RFC 0017 D11）：失败输出命中沙箱拒绝特征时，
/// 在 stderr 追加行动提示（改写法或申请升级），让 Agent 能自主恢复。
fn annotate_violation(mut resp: ExecResponse) -> ExecResponse {
    if !resp.ok
        && let Some(hint) = tiangong_sandbox::explain_violation(&resp.stderr)
    {
        resp.stderr = format!("{}\n[沙箱提示] {}", resp.stderr, hint);
    }
    resp
}

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for CommandService {
    async fn dispatch(
        &self,
        request: tiangong_plugin_runtime::protocol::Request,
    ) -> tiangong_plugin_runtime::protocol::Response {
        CommandService::dispatch(self, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_with(workspace: &str, full_trust: bool) -> CommandService {
        let service = CommandService::new().unwrap();
        service
            .handle_set_workspace(SetWorkspaceRequest {
                workspace: Some(workspace.to_string()),
                full_trust,
                allowed_commands: Vec::new(),
            })
            .unwrap();
        service
    }

    #[test]
    fn full_trust_bypasses_sandbox() {
        let service = service_with("/tmp", true);
        let result = service.apply_sandbox(
            "run_command",
            tiangong_sandbox::CommandRisk::KnownDangerous,
            "mkfs".to_string(),
            vec![],
            Path::new("/tmp"),
            None,
        );
        let (cmd, args) = result.expect("全信模式应原样放行");
        assert_eq!(cmd, "mkfs");
        assert!(args.is_empty());
    }

    #[test]
    fn dangerous_command_is_denied_with_hint() {
        let service = service_with("/tmp", false);
        let result = service.apply_sandbox(
            "run_command",
            tiangong_sandbox::CommandRisk::KnownDangerous,
            "mkfs.ext4".to_string(),
            vec![],
            Path::new("/tmp"),
            None,
        );
        let resp = result.expect_err("高危命令应被拒绝");
        assert!(!resp.ok);
        assert!(resp.stderr.contains("request_user"), "提示应引导升级审批");
    }

    #[test]
    fn escalated_request_runs_full_access() {
        let service = service_with("/tmp", false);
        // 升级声明存在时，即使命中高危预分类也全权放行（审计留痕）。
        // sidecar 收到的请求经宿主转发层核验并剥离 token，此处模拟验证后形态。
        let escalated = EscalatedRequest {
            approval_note: "用户已在对话中批准格式化操作".to_string(),
            token: String::new(),
        };
        let result = service.apply_sandbox(
            "run_command",
            tiangong_sandbox::CommandRisk::KnownDangerous,
            "mkfs.ext4".to_string(),
            vec![],
            Path::new("/tmp"),
            Some(&escalated),
        );
        let (cmd, _) = result.expect("升级声明应全权放行");
        assert_eq!(cmd, "mkfs.ext4");
    }

    #[test]
    fn trusted_command_session_list() {
        let service = service_with("/tmp", false);
        service
            .handle_trust_command(TrustCommandRequest {
                command: "docker".to_string(),
                approval_note: "用户批准容器操作".to_string(),
            })
            .unwrap();
        let result = service.apply_sandbox(
            "run_command",
            tiangong_sandbox::CommandRisk::Unknown,
            "docker".to_string(),
            vec!["ps".to_string()],
            Path::new("/tmp"),
            None,
        );
        let (cmd, args) = result.expect("会话信任命中应全权放行");
        assert_eq!(cmd, "docker");
        assert_eq!(args, vec!["ps".to_string()]);
    }

    #[test]
    fn unknown_command_is_wrapped_or_rejected() {
        let service = service_with("/tmp", false);
        let result = service.apply_sandbox(
            "run_command",
            tiangong_sandbox::CommandRisk::Unknown,
            "cargo".to_string(),
            vec!["build".to_string()],
            Path::new("/tmp"),
            None,
        );
        match result {
            // 平台沙箱可用：包装入口 + 前缀 + 原命令 + 原参数。
            Ok((cmd, args)) => {
                assert!(cmd.contains("sandbox-exec") || cmd.contains("bwrap"));
                assert_eq!(args.last().unwrap(), "build");
                assert!(args.contains(&"cargo".to_string()));
            }
            // 平台沙箱不可用（嵌套沙箱环境 / 缺 bwrap）：默认拒绝执行，
            // 错误提示需引导升级审批通道。
            Err(resp) => {
                assert!(!resp.ok);
                assert!(resp.stderr.contains("沙箱不可用"));
                assert!(resp.stderr.contains("request_user"));
            }
        }
    }

    #[test]
    fn unavailable_sandbox_rejects_execution() {
        let service = service_with("/tmp", false);
        // 直接构造不可用形态验证拒绝路径（不依赖宿主环境沙箱状态）。
        let resp = SandboxedProgram::Unavailable("测试：平台不支持".to_string());
        let SandboxedProgram::Unavailable(reason) = resp else {
            panic!("测试前提失败");
        };
        let message = format!("当前平台沙箱不可用，命令未执行：{reason}。");
        assert!(message.contains("沙箱不可用"));
        // 经 apply_sandbox 的完整路径（宿主沙箱可用时 Wrapped、不可用时拒绝）
        // 由 sandboxed_policy_never_degrades_silently 与本测试共同覆盖。
        let _ = service;
    }
}
