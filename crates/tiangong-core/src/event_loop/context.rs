//! 事件到上下文的转换逻辑
//!
//! 将 LoopEvent 转换为消息注入到循环上下文中。

use crate::session::{Message, MessageRole, now_text};

use super::types::LoopEvent;

/// 将事件转换为上下文消息
///
/// 返回 None 表示该事件不产生上下文消息（如 Cancel、SystemSignal）。
pub fn event_to_message(event: &LoopEvent) -> Option<Message> {
    match event {
        LoopEvent::UserMessage { content } => Some(Message {
            id: scru128::new().to_string(),
            role: MessageRole::User,
            content: content.clone(),
            reasoning_content: String::new(),
            worker_id: None,
            created_at: now_text(),
        }),

        LoopEvent::ToolResult { call_name, result } => {
            let feedback = format!(
                "工具 {} 执行{}：{}",
                call_name,
                if result.ok { "成功" } else { "失败" },
                if result.stdout.chars().count() > 2000 {
                    let truncated: String = result.stdout.chars().take(2000).collect();
                    format!("{truncated}...(截断)")
                } else if result.stdout.is_empty() {
                    result.summary.clone()
                } else {
                    result.stdout.clone()
                }
            );
            Some(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: feedback,
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            })
        }

        LoopEvent::BackgroundTaskDone {
            task_id,
            summary,
            success,
        } => {
            let status = if *success { "完成" } else { "失败" };
            Some(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: format!("后台任务 {task_id} {status}：{summary}"),
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            })
        }

        LoopEvent::PermissionResponse {
            request_id,
            approved,
        } => {
            let decision = if *approved { "已批准" } else { "已拒绝" };
            Some(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: format!("权限审批 {request_id}：{decision}"),
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            })
        }

        // Cancel 和 SystemSignal 不产生上下文消息
        LoopEvent::Cancel | LoopEvent::SystemSignal(_) => None,
    }
}

/// 批量将事件转换为上下文消息
pub fn events_to_messages(events: &[LoopEvent]) -> Vec<Message> {
    events.iter().filter_map(event_to_message).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolResult;

    #[test]
    fn user_message_to_context() {
        let event = LoopEvent::UserMessage {
            content: "你好".into(),
        };
        let msg = event_to_message(&event).unwrap();
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content, "你好");
    }

    #[test]
    fn tool_result_to_context() {
        let event = LoopEvent::ToolResult {
            call_name: "read_file".into(),
            result: ToolResult {
                ok: true,
                summary: "读取成功".into(),
                stdout: "file content".into(),
                stderr: String::new(),
                exit_code: 0,
                execution: None,
            },
        };
        let msg = event_to_message(&event).unwrap();
        assert_eq!(msg.role, MessageRole::System);
        assert!(msg.content.contains("read_file"));
        assert!(msg.content.contains("成功"));
    }

    #[test]
    fn cancel_no_message() {
        assert!(event_to_message(&LoopEvent::Cancel).is_none());
    }

    #[test]
    fn batch_conversion() {
        let events = vec![
            LoopEvent::UserMessage {
                content: "hello".into(),
            },
            LoopEvent::Cancel,
            LoopEvent::BackgroundTaskDone {
                task_id: "t1".into(),
                summary: "done".into(),
                success: true,
            },
        ];
        let msgs = events_to_messages(&events);
        assert_eq!(msgs.len(), 2); // Cancel 被过滤
    }
}
