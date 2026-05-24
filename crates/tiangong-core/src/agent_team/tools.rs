use crate::model::ToolSpec;

/// 团队工具最大 Agent 数量
pub const MAX_AGENTS: usize = 8;
/// 同时运行的 Sub Agent 数量上限
pub const MAX_CONCURRENT_SUB_AGENTS: usize = 4;
/// Sub Agent 最大执行轮次
pub const SUB_AGENT_MAX_ROUNDS: usize = 10;
/// Sub Agent 共享的 token 总预算（所有 Sub Agent 累计不超过此值）
pub const SUB_AGENT_TOTAL_TOKEN_BUDGET: usize = 200_000;

/// 注册所有团队协作工具
pub fn inject_agent_team_tools(tools: &mut Vec<ToolSpec>) {
    let team_tools = [
        create_agent_tool(),
        dismiss_agent_tool(),
        send_message_tool(),
        broadcast_message_tool(),
        notify_user_tool(),
        lock_file_tool(),
        unlock_file_tool(),
    ];

    for tool in team_tools {
        if !tools.iter().any(|t| t.name == tool.name) {
            tools.push(tool);
        }
    }
}

fn create_agent_tool() -> ToolSpec {
    ToolSpec {
        name: "create_agent".to_string(),
        description: "创建一个 Sub Agent 加入团队。Agent 拥有独立的执行上下文和指定角色，持续存在直到被解散。"
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "role": {
                    "type": "string",
                    "description": "Agent 角色标识，用于 @提及（如 'pm'、'dev'、'test'）"
                },
                "label": {
                    "type": "string",
                    "description": "Agent 显示名称（如 'Project Manager'、'Developer'）"
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Agent 的角色系统提示，定义其职责和行为规范"
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Agent 可用的工具列表。不指定时继承你的全部工具（不含 create_agent/dismiss_agent）。建议根据任务需要精确授权。"
                }
            },
            "required": ["role", "label", "system_prompt"]
        }),
    }
}

fn dismiss_agent_tool() -> ToolSpec {
    ToolSpec {
        name: "dismiss_agent".to_string(),
        description: "解散指定的 Sub Agent，释放其持有的所有资源（文件锁等）。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "role": {
                    "type": "string",
                    "description": "要解散的 Agent 角色"
                }
            },
            "required": ["role"]
        }),
    }
}

fn send_message_tool() -> ToolSpec {
    ToolSpec {
        name: "send_message".to_string(),
        description: "向指定 Agent 发送消息。支持 @role 格式指定目标。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "目标 Agent 的 role"
                },
                "content": {
                    "type": "string",
                    "description": "消息内容"
                },
                "priority": {
                    "type": "string",
                    "enum": ["normal", "urgent"],
                    "description": "消息优先级，默认 normal"
                }
            },
            "required": ["to", "content"]
        }),
    }
}

fn broadcast_message_tool() -> ToolSpec {
    ToolSpec {
        name: "broadcast_message".to_string(),
        description: "向所有存活 Agent 广播消息。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "广播内容"
                },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "排除的 Agent role 列表（通常排除自己）"
                }
            },
            "required": ["content"]
        }),
    }
}

fn notify_user_tool() -> ToolSpec {
    ToolSpec {
        name: "notify_user".to_string(),
        description: "直接向用户推送消息，无需经主 Agent 转发。用于进度汇报、阻塞通知、提问等场景。推送消息会携带 Agent 标识。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "推送给用户的内容"
                },
                "level": {
                    "type": "string",
                    "enum": ["info", "warning", "error", "question"],
                    "description": "消息级别，默认 info"
                }
            },
            "required": ["content"]
        }),
    }
}

fn lock_file_tool() -> ToolSpec {
    ToolSpec {
        name: "lock_file".to_string(),
        description: "获取文件编辑锁。编辑文件前必须先获取锁，防止多 Agent 冲突。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要锁定的文件路径"
                }
            },
            "required": ["path"]
        }),
    }
}

fn unlock_file_tool() -> ToolSpec {
    ToolSpec {
        name: "unlock_file".to_string(),
        description: "释放文件编辑锁。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要释放的文件路径"
                }
            },
            "required": ["path"]
        }),
    }
}
