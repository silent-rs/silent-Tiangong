//! Command sidecar 业务服务：按操作名分发请求，承载 tokio 子进程 spawn 与命令校验策略。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow};

use tiangong_plugin_command_protocol::exec::{
    ExecResponse, RUN_COMMAND_OPERATION, RUN_SHELL_OPERATION, RunCommandRequest, RunShellRequest,
    SET_WORKSPACE_OPERATION, SetWorkspaceRequest,
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
        // 沙箱与审批由宿主透明封套处理（RFC 0017 透明执行封套）：本进程可能
        // 已被宿主以一次性沙箱实例方式启动，插件不做任何沙箱决策。
        exec::exec_and_collect(&cmd, &cmd_args, &effective_cwd, timeout_ms)
            .await
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
        exec::exec_and_collect(&cmd, &cmd_args, &effective_cwd, timeout_ms)
            .await
            .unwrap_or_else(|e| error_response("命令执行", e))
    }

    // ── 生命周期 ─────────────────────────────────────────────

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

#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for CommandService {
    async fn dispatch(
        &self,
        request: tiangong_plugin_runtime::protocol::Request,
    ) -> tiangong_plugin_runtime::protocol::Response {
        CommandService::dispatch(self, request).await
    }
}
