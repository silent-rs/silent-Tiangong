/// 父 Core 中的完整 Agent Team 插件 ID。
pub const PLUGIN_ID: &str = "agent_team";
/// 子 Core 中受限团队客户端的插件 ID。
pub(crate) const CHILD_PLUGIN_ID: &str = "agent_team_child";
/// 工具中用于寻址父 Core 的稳定角色名；实际 actor ID 是父 Session ID。
pub(crate) const MAIN_ROLE: &str = "main";
pub const MAX_AGENTS: usize = 8;

pub const TOOL_CREATE_AGENT: &str = "create_agent";
pub const TOOL_DISMISS_AGENT: &str = "dismiss_agent";
pub const TOOL_SEND_MESSAGE: &str = "send_message";
pub const TOOL_BROADCAST_MESSAGE: &str = "broadcast_message";
pub const TOOL_NOTIFY_USER: &str = "notify_user";

pub fn is_team_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_CREATE_AGENT
            | TOOL_DISMISS_AGENT
            | TOOL_SEND_MESSAGE
            | TOOL_BROADCAST_MESSAGE
            | TOOL_NOTIFY_USER
    )
}
