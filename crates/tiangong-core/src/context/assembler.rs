//! 工具按意图过滤的辅助函数。
//!
//! 历史上本模块曾承载 `ContextAssembler` / `QueryClassifier` / `QueryMode`
//! 这套「查询意图分类 → 工具注入」流水线，但该流水线从未真正接入主执行链路：
//! `QueryClassifier::classify` 固定返回 `MultiStepExecution`，实际路由由 ReAct
//! 主循环的 LLM 在完整上下文下自行判断。随旧架构退场，这些伪分类概念已删除，
//! 仅保留真正被 `ReactEngine` 使用的后台任务工具过滤逻辑。
//!
//! 后台任务工具（spawn_task / query_task / list_tasks / cancel_task /
//! wait_tasks）仅在用户输入表达「后台 / 并行 / 长期运行」等意图时才注入，
//! 避免常规命令被这些工具干扰。

use crate::model::ToolSpec;

pub(crate) fn is_background_task_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_task" | "query_task" | "list_tasks" | "cancel_task" | "wait_tasks"
    )
}

pub(crate) fn should_expose_background_task_tools(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    [
        "后台",
        "不阻塞",
        "并行",
        "同时执行",
        "持续运行",
        "长期运行",
        "服务",
        "监听",
        "启动 server",
        "启动服务",
        "dev server",
        "background",
        "non-blocking",
        "parallel",
        "concurrent",
        "daemon",
        "server",
        "watch",
        "long-running",
        "keep running",
        "background task",
        "后台任务",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(crate) fn filter_background_task_tools(
    tools: Vec<ToolSpec>,
    user_input: &str,
) -> Vec<ToolSpec> {
    if should_expose_background_task_tools(user_input) {
        return tools;
    }
    tools
        .into_iter()
        .filter(|tool| !is_background_task_tool(&tool.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolSpec;

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn normal_command_input_hides_background_task_tools() {
        let tools = vec![tool("run_shell"), tool("spawn_task"), tool("wait_tasks")];
        let names: Vec<_> = filter_background_task_tools(tools, "执行 git diff 看一下改动")
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, vec!["run_shell"]);
    }

    #[test]
    fn background_intent_keeps_background_task_tools() {
        let tools = vec![tool("run_shell"), tool("spawn_task"), tool("wait_tasks")];
        let names: Vec<_> = filter_background_task_tools(tools, "后台启动 dev server，不要阻塞")
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, vec!["run_shell", "spawn_task", "wait_tasks"]);
    }
}
