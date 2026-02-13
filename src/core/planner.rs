use serde::{Deserialize, Serialize};

use crate::core::agent_config::AgentConfig;
use crate::core::mcp::build_mcp_hints;
use crate::core::skills::build_skill_hints;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub id: String,
    pub objective: String,
    pub summary: String,
    pub steps: Vec<PlanStep>,
    pub risks: Vec<String>,
    pub verify_commands: Vec<String>,
    pub skill_hints: Vec<String>,
    pub mcp_hints: Vec<String>,
}

pub fn build_minimal_plan(user_input: &str, agent_config: &AgentConfig) -> TaskPlan {
    let has_tool_intent =
        user_input.contains("读取") || user_input.contains("文件") || user_input.contains("目录");
    let has_command_intent = user_input.contains("命令");
    let has_code_intent = user_input.contains("代码")
        || user_input.contains("rust")
        || user_input.contains("Rust")
        || user_input.contains(".rs");

    let step = if user_input.contains("读取") || user_input.contains("文件") {
        PlanStep {
            id: new_id(),
            name: "prepare_tool_call".to_string(),
            description: "准备工具调用参数并进入执行阶段".to_string(),
        }
    } else {
        PlanStep {
            id: new_id(),
            name: "generate_response".to_string(),
            description: "基于上下文生成回答".to_string(),
        }
    };

    let objective = if has_tool_intent || has_command_intent {
        "通过最小工具链完成信息收集后生成回答".to_string()
    } else {
        "基于上下文直接生成高质量回答".to_string()
    };

    let risks = if has_command_intent {
        vec![
            "命令执行可能失败或被权限策略拦截".to_string(),
            "命令输出可能被截断，需要二次确认".to_string(),
        ]
    } else if has_tool_intent {
        vec![
            "目标路径可能不存在或越界".to_string(),
            "读取内容可能过长导致摘要丢失细节".to_string(),
        ]
    } else {
        vec!["上下文信息不足时回答可能不完整".to_string()]
    };

    let verify_commands = if has_command_intent || has_code_intent {
        let mut commands = vec!["cargo check --workspace".to_string()];
        if user_input.contains("clippy") || user_input.contains("严格") {
            commands.push(
                "cargo clippy --workspace --all-targets --tests --benches -- -D warnings"
                    .to_string(),
            );
        }
        commands
    } else {
        vec!["无".to_string()]
    };

    let skill_hints = build_skill_hints(user_input, &agent_config.skills);
    let mcp_hints = build_mcp_hints(user_input, &agent_config.mcp);

    TaskPlan {
        id: new_id(),
        objective,
        summary: format!("针对输入“{}”生成最小执行计划", user_input),
        steps: vec![step],
        risks,
        verify_commands,
        skill_hints,
        mcp_hints,
    }
}

fn new_id() -> String {
    scru128::new().to_string()
}
