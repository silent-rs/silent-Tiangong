//! Memory Recall 工具注入、执行与结果构造
//!
//! 提供 recall_memory / analyze_attachment 两类工具的注入、执行和结果序列化。

use crate::memory::turn_result::compact_single_memory_text;
use crate::model::ToolSpec;
use crate::react::message::latest_user_message;
use crate::session::{MessageRole, Session};
use tiangong_types::TokenUsage;

pub(crate) type MemoryRecallToolOutput = (crate::tool::ToolResult, TokenUsage, bool);

// ── recall_memory 工具注入 ──

pub(crate) fn inject_memory_recall_tool(tools: &mut Vec<ToolSpec>) {
    if tools.iter().any(|tool| tool.name == "recall_memory") {
        return;
    }

    tools.push(ToolSpec {
        name: "recall_memory".to_string(),
        description: "按需回忆历史上下文、跨会话结果、之前的工具输出或生成产物。用户提到刚刚、刚才、上次、之前、那个、继续、这张图、生成的图片等历史指代时，应先调用此工具。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "要回忆的内容，结合用户当前请求改写成可检索查询"
                },
                "reason": {
                    "type": "string",
                    "description": "为什么需要回忆，简述当前任务依赖的历史语境"
                },
                "expected": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "期望找回的内容类型，如 media、file、tool_result、decision、code_context"
                },
                "limit": {
                    "type": "integer",
                    "description": "最多返回多少条记忆，默认 5，最大 10"
                }
            },
            "required": ["query"]
        }),
    });
}

// ── recall_memory 工具执行 ──

pub(crate) async fn execute_memory_recall_tool(
    call: &crate::model::ToolCall,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
    session: &Session,
) -> MemoryRecallToolOutput {
    let started = std::time::Instant::now();
    let Some(handle) = memory_handle else {
        return (
            memory_recall_tool_result(
                false,
                "记忆系统未启用",
                String::new(),
                "memory disabled".to_string(),
                1,
                Vec::new(),
                started,
            ),
            tiangong_types::TokenUsage::default(),
            false,
        );
    };

    let query = call
        .arguments
        .get("query")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| latest_user_message(session));
    if query.is_empty() {
        return (
            memory_recall_tool_result(
                false,
                "缺少回忆查询",
                String::new(),
                "recall_memory.query is empty".to_string(),
                1,
                Vec::new(),
                started,
            ),
            tiangong_types::TokenUsage::default(),
            false,
        );
    }

    let reason = call
        .arguments
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    let expected = call
        .arguments
        .get("expected")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let limit = call
        .arguments
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 10) as usize;

    let response = handle
        .recall_context(tiangong_memory::MemoryRecallRequest {
            query: query.to_string(),
            reason: (!reason.is_empty()).then(|| reason.to_string()),
            expected,
            context: build_memory_recall_context(session),
            limit,
        })
        .await;

    let memory_usage = tiangong_types::TokenUsage::from(response.usage.clone());

    if response.hits.is_empty() {
        return (
            memory_recall_tool_result(
                true,
                "未找到相关记忆",
                if response.content.trim().is_empty() {
                    format!("未找到与「{query}」相关的历史记忆。")
                } else {
                    response.content
                },
                String::new(),
                0,
                vec![query.to_string()],
                started,
            ),
            memory_usage,
            false,
        );
    }

    let stdout = if response.content.trim().is_empty() {
        "没有发现当前上下文之外的增量记忆。".to_string()
    } else {
        response.content
    };
    let allow_memory_context = !stdout
        .trim()
        .starts_with("没有发现当前上下文之外的增量记忆");

    (
        memory_recall_tool_result(
            true,
            format!("命中 {} 条相关记忆并完成整理", response.hits.len()),
            stdout,
            String::new(),
            0,
            vec![query.to_string()],
            started,
        ),
        memory_usage,
        allow_memory_context,
    )
}

fn memory_recall_tool_result(
    ok: bool,
    summary: impl Into<String>,
    stdout: String,
    stderr: String,
    exit_code: i32,
    args: Vec<String>,
    started: std::time::Instant,
) -> crate::tool::ToolResult {
    let summary = summary.into();
    crate::tool::ToolResult {
        ok,
        summary: summary.clone(),
        stdout,
        stderr,
        exit_code,
        execution: Some(crate::tool::ToolExecutionRecord {
            tool_name: "recall_memory".to_string(),
            args,
            duration_ms: started.elapsed().as_millis() as u64,
            ok,
            exit_code,
            summary,
        }),
    }
}

// ── analyze_attachment 工具执行 ──

// ── 重复调用拦截 ──

pub(crate) fn duplicate_memory_recall_tool_result() -> MemoryRecallToolOutput {
    let started = std::time::Instant::now();
    (
        memory_recall_tool_result(
            true,
            "本轮已完成回忆，跳过重复调用",
            "recall_memory 本轮已经执行过，回忆结果已经注入当前上下文。请直接基于已有回忆结果完成用户原始目标，不要再次调用 recall_memory。"
                .to_string(),
            String::new(),
            0,
            vec!["duplicate-recall".to_string()],
            started,
        ),
        tiangong_types::TokenUsage::default(),
        false,
    )
}

// ── 上下文构建辅助 ──

fn build_memory_recall_context(session: &Session) -> Vec<String> {
    let mut items = session
        .messages
        .iter()
        .rev()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => return None,
                MessageRole::Tool => message.tool_name.as_deref().unwrap_or("tool"),
            };
            let content = compact_single_memory_text(&message.content, 900);
            (!content.is_empty()).then(|| format!("{role}: {content}"))
        })
        .take(30)
        .collect::<Vec<_>>();
    items.reverse();
    items
}
