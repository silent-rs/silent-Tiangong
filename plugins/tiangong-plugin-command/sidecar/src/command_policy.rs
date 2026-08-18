//! 命令执行策略（沙箱预留点 A）。
//!
//! 当前 command 的校验散落在 toolkit 自由函数里（validate_command /
//! validate_shell_command_args / resolve_effective_cwd_with），且 command 与
//! terminal 各自复制了 split_command 和 validate 包装。本模块把它收敛成一个
//! 可替换策略 trait——当前唯一实现 `TrustModeCommandPolicy` 包 toolkit 调用；
//! 未来引入更强沙箱（命令 AST 化、env 黑名单、网络出口控制）时，只需提供新的
//! `CommandPolicy` 实现，业务代码（service）不动。
//!
//! 这层抽象也为未来 runtime 层的进程级沙箱（landlock/seccomp）留出对接面：
//! 当 runtime 能下发沙箱配置时，`CommandPolicy` 实现可接收该配置，把"允许的
//! 命令/网络出口/env key"应用到校验上。

use std::path::{Path, PathBuf};

use anyhow::Result;
use tiangong_plugin_command_protocol::CommandAccessContext;
use tiangong_toolkit as shared;

/// 命令执行策略：把"访问能力 + 命令"校验为是否允许执行。
///
/// 当前唯一实现是 [`TrustModeCommandPolicy`]；未来沙箱实现可替换本 trait。
/// `full_trust` 由策略内部处理（FullTrust 跳过校验，与原进程内实现一致）。
pub trait CommandPolicy: Send + Sync {
    /// 校验 run_command（非 shell）：白名单 + 路径越界 + shell 形式。
    /// 仅对硬性拒绝条件（forbidden token、路径越界、shell 形式不合法）报错；
    /// 白名单结果只作为风险信息，不在本策略内触发用户征询。
    fn validate_run_command(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        ctx: &CommandAccessContext,
    ) -> Result<()>;

    /// 校验 run_shell：shell 形式 + forbidden token + 重定向 + 脚本拆分 + 路径越界。
    fn validate_run_shell(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        ctx: &CommandAccessContext,
    ) -> Result<()>;

    /// 解析 cwd（含越界校验）。
    fn resolve_cwd(&self, raw: Option<&str>, base: &Path) -> Result<PathBuf>;
}

/// 基于信任模式的命令策略（当前唯一实现，对齐原进程内 command 插件语义）。
///
/// - `full_trust` 为 true：跳过所有校验（与原实现 `if !self.is_full_trust()` 一致）。
/// - `full_trust` 为 false：调 toolkit 的 validate_command / validate_shell_command_args
///   / resolve_effective_cwd_with 做完整校验。
pub struct TrustModeCommandPolicy;

impl TrustModeCommandPolicy {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TrustModeCommandPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPolicy for TrustModeCommandPolicy {
    fn validate_run_command(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        ctx: &CommandAccessContext,
    ) -> Result<()> {
        if ctx.full_trust {
            return Ok(());
        }
        // 复用原进程内 validate_command 包装逻辑：shell 形式走 shell 校验，
        // 否则走路径越界 + 白名单。白名单外命令返回 NeedsApproval 但不报错。
        let _ = validate_command(cmd, args, cwd, &ctx.allowed_commands)?;
        Ok(())
    }

    fn validate_run_shell(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        ctx: &CommandAccessContext,
    ) -> Result<()> {
        if ctx.full_trust {
            return Ok(());
        }
        let _ = shared::validate_shell_command_args(cmd, args, cwd, &ctx.allowed_commands)?;
        Ok(())
    }

    fn resolve_cwd(&self, raw: Option<&str>, base: &Path) -> Result<PathBuf> {
        shared::resolve_effective_cwd_with(raw, base)
    }
}

/// 校验命令（非 shell）：白名单 + 路径越界。
///
/// 返回 [`shared::CommandValidation`] 表示白名单校验结果；`Err` 仅用于硬性拒绝
///（forbidden tokens、路径越界、shell 形式不合法）。与原 command 插件
/// handler.rs 的 validate_command 一致。
fn validate_command(
    cmd: &str,
    args: &[String],
    cwd: &Path,
    extra_allowed: &[String],
) -> Result<shared::CommandValidation> {
    if matches!(cmd, "bash" | "sh" | "powershell" | "pwsh") {
        shared::validate_shell_command_args(cmd, args, cwd, extra_allowed)
    } else {
        shared::validate_command_args_in_allowed_roots(cmd, args, cwd)?;
        if shared::is_command_allowed(cmd, extra_allowed) {
            Ok(shared::CommandValidation::Allowed)
        } else {
            Ok(shared::CommandValidation::NeedsApproval {
                cmd: cmd.to_string(),
            })
        }
    }
}
