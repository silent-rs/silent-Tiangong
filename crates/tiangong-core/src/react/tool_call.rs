//! 单个工具调用的启动逻辑。

use std::future::Future;
use std::pin::Pin;

use crate::model::ToolCall;
use crate::tool::ToolResult;
use crate::turn_context::TurnContext;

/// 从工具参数中提取 Agent 指定的超时（毫秒）。
///
/// Agent 可在工具参数里用 `timeout` 字段覆盖默认超时（如 `{"timeout": 30000, ...}`）。
/// 未指定或非法时返回 `None`，由调用方回退到默认值。
fn agent_specified_timeout_ms(arguments: &serde_json::Value) -> Option<u64> {
    let timeout = arguments.get("timeout")?.as_u64()?;
    (timeout > 0).then_some(timeout)
}

/// 根据本轮工具覆盖表启动一个工具调用。
///
/// 统一在 Core 层对工具执行做超时包装：优先用 Agent 在参数里指定的 `timeout`
/// （毫秒），否则用 `ctx.tool_timeout_ms` 默认值。超时后返回明确的超时结果，
/// 避免插件侧无工具级超时（WASM 插件仅靠 fuel/epoch 兜底）导致执行悬挂。
pub(super) fn start_tool_call(
    ctx: &mut TurnContext,
    call: &ToolCall,
    actor_id: &str,
) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> {
    let timeout_ms = agent_specified_timeout_ms(&call.arguments).unwrap_or(ctx.tool_timeout_ms);
    let tool_name = call.name.clone();

    if let Some(handler) = ctx.tool_overrides.get(&call.name).cloned() {
        let handler_future = handler.handle(call, &mut ctx.session, actor_id);
        return Box::pin(async move {
            match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                handler_future,
            )
            .await
            {
                Ok(Some(result)) => result,
                Ok(None) => unregistered_tool_result(tool_name),
                Err(_) => tool_timeout_result(tool_name, timeout_ms),
            }
        });
    }

    Box::pin(async move { unregistered_tool_result(tool_name) })
}

fn unregistered_tool_result(tool_name: String) -> ToolResult {
    ToolResult {
        ok: false,
        summary: format!("未注册的工具：{tool_name}（请确认对应插件已启用）"),
        stdout: String::new(),
        stderr: format!("tool {tool_name} not handled by any plugin"),
        exit_code: 1,
        execution: None,
    }
}

fn tool_timeout_result(tool_name: String, timeout_ms: u64) -> ToolResult {
    ToolResult {
        ok: false,
        summary: format!("工具 {tool_name} 执行超时（{timeout_ms}ms）"),
        stdout: String::new(),
        stderr: format!("tool {tool_name} timed out after {timeout_ms}ms"),
        exit_code: 1,
        execution: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_timeout_提取有效值() {
        assert_eq!(agent_specified_timeout_ms(&json!({"timeout": 30000})), Some(30000));
        assert_eq!(agent_specified_timeout_ms(&json!({"timeout": 1})), Some(1));
    }

    #[test]
    fn agent_timeout_缺省或非法返回none() {
        assert_eq!(agent_specified_timeout_ms(&json!({})), None);
        assert_eq!(agent_specified_timeout_ms(&json!({"timeout": 0})), None);
        assert_eq!(agent_specified_timeout_ms(&json!({"timeout": "abc"})), None);
        assert_eq!(agent_specified_timeout_ms(&json!({"timeout": -5})), None);
    }

    #[test]
    fn agent_timeout_不影响其他参数() {
        assert_eq!(
            agent_specified_timeout_ms(&json!({"cmd": "ls", "timeout": 5000})),
            Some(5000)
        );
    }

    #[test]
    fn timeout_result_结构正确() {
        let result = tool_timeout_result("test_tool".to_string(), 30000);
        assert!(!result.ok);
        assert_eq!(result.exit_code, 1);
        assert!(result.summary.contains("test_tool"));
        assert!(result.summary.contains("30000"));
    }
}
