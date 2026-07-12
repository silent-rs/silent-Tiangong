//! 团队工具常量（迁自 `tiangong-core/src/agent_team/tools.rs`）。

/// 插件在 Core 通用持久投递与控制合同中的稳定 ID。
pub const PLUGIN_ID: &str = "agent_team";
/// 取消指定角色执行的插件控制动作。
pub const CONTROL_CANCEL_AGENT: &str = "cancel_agent";

/// 团队工具最大 Agent 数量
pub const MAX_AGENTS: usize = 8;
/// 同时运行的 Sub Agent 数量上限
pub const MAX_CONCURRENT_SUB_AGENTS: usize = 4;
/// Sub Agent 单次工具执行阶段（ReAct Loop 内层）的最大轮次
pub const SUB_AGENT_MAX_TOOL_ROUNDS: usize = 8;
/// Sub Agent 总结后重新进入工具执行阶段的最大次数
pub const SUB_AGENT_MAX_OUTER_ITERATIONS: u32 = 2;
/// Sub Agent 共享的 token 总预算（所有 Sub Agent 累计不超过此值）
pub const SUB_AGENT_TOTAL_TOKEN_BUDGET: usize = 200_000;
/// Sub Agent 文件锁租期。
pub(crate) const FILE_LOCK_LEASE_SECS: i64 = 300;
/// 前台命令上限：比锁租期短 60 秒，为中止与收尾留出余量。
pub(crate) const MAX_SUB_AGENT_COMMAND_TIMEOUT_SECS: u64 = FILE_LOCK_LEASE_SECS as u64 - 60;

/// 团队工具名常量。
pub const TOOL_CREATE_AGENT: &str = "create_agent";
pub const TOOL_DISMISS_AGENT: &str = "dismiss_agent";
pub const TOOL_SEND_MESSAGE: &str = "send_message";
pub const TOOL_BROADCAST_MESSAGE: &str = "broadcast_message";
pub const TOOL_NOTIFY_USER: &str = "notify_user";
pub const TOOL_LOCK_FILE: &str = "lock_file";
pub const TOOL_UNLOCK_FILE: &str = "unlock_file";

/// 判断是否为团队协作工具。
pub fn is_team_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_CREATE_AGENT
            | TOOL_DISMISS_AGENT
            | TOOL_SEND_MESSAGE
            | TOOL_BROADCAST_MESSAGE
            | TOOL_NOTIFY_USER
            | TOOL_LOCK_FILE
            | TOOL_UNLOCK_FILE
    )
}
