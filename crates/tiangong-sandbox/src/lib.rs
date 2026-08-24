//! 沙箱执行层（RFC 0017，任务边界收敛版）。
//!
//! 职责：命令套壳沙箱——策略编译（macOS Seatbelt / Linux bubblewrap）、
//! 命令包装、平台能力探测、Windows 明确拒绝、fail-closed、
//! 宿主策略表与沙箱程序（bin）。
//!
//! 已拆分到独立分支：
//! - 插件信任模型 → feature/plugin-trust-model
//! - 工作区快照恢复 → feature/workspace-snapshots
//! - 无沙箱审批升级 → feature/sandbox-escalation

pub mod host_policy;
pub mod launcher_manager;
pub mod sandbox;

/// Windows 上通过进程环境传递一次性 Launcher 请求；Launcher 启动目标前会移除。
/// Unix 使用 fd3，不使用该变量。
pub const POLICY_ENV: &str = "TIANGONG_SANDBOX_REQUEST";

pub use host_policy::{HostExecutionPolicy, SidecarTransport};
pub use sandbox::{
    SandboxAvailability, SandboxMode, SandboxPolicy, SandboxedProgram, availability,
    explain_violation, wrap,
};
