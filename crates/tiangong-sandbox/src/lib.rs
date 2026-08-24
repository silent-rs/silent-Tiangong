//! 沙箱执行层（RFC 0017，任务边界收敛版）。
//!
//! 职责：命令套壳沙箱——策略编译（macOS Seatbelt / Linux bubblewrap /
//! Windows 受限令牌占位）、命令包装、平台能力探测、fail-closed、
//! 宿主策略表与沙箱程序（bin）。
//!
//! 已拆分到独立分支：
//! - 插件信任模型 → feature/plugin-trust-model
//! - 工作区快照恢复 → feature/workspace-snapshots
//! - 无沙箱审批升级 → feature/sandbox-escalation

pub mod host_policy;
pub mod launcher_manager;
pub mod sandbox;

pub use host_policy::{HostExecutionPolicy, SidecarTransport};
pub use sandbox::{
    CommandRisk, SandboxAvailability, SandboxMode, SandboxPolicy, SandboxedProgram, assess_program,
    assess_script, availability, denial_hint, explain_violation, wrap,
};
