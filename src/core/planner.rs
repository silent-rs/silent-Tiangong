use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub id: String,
    pub summary: String,
    pub steps: Vec<PlanStep>,
}

pub fn build_minimal_plan(user_input: &str) -> TaskPlan {
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

    TaskPlan {
        id: new_id(),
        summary: format!("针对输入“{}”生成最小执行计划", user_input),
        steps: vec![step],
    }
}

fn new_id() -> String {
    scru128::new().to_string()
}
