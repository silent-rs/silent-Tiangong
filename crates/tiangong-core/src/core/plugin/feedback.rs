//! 插件到 Core 的反馈通道。
//!
//! 会话注入、用量上报和流事件都通过当前 turn 的命令通道投递，由 Agent Loop
//! 按命令顺序处理。插件不持有 Session、用量收集器或前端事件发送端。

use tokio::sync::mpsc::UnboundedSender;

use crate::core::command::Command;

/// 插件向 Core 投递的会话注入反馈。
#[derive(Debug, Clone)]
pub struct PluginFeedback {
    pub tool_name: String,
    pub payload: serde_json::Value,
}

impl PluginFeedback {
    pub fn new(tool_name: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            payload,
        }
    }
}

/// 插件状态反馈通道的发送端。
///
/// 该发送端只属于当前 turn；turn 结束后接收端随任务释放，迟到反馈会发送失败，
/// 不会进入后续 turn。
#[derive(Clone)]
pub struct PluginFeedbackTx {
    tx: UnboundedSender<Command>,
}

impl PluginFeedbackTx {
    pub(crate) fn new(tx: UnboundedSender<Command>) -> Self {
        Self { tx }
    }

    /// 将插件产生的结构化内容注入当前会话。
    pub fn inject_tool(&self, tool_name: impl Into<String>, payload: serde_json::Value) {
        let _ = self.tx.send(Command::InjectTool {
            tool_name: tool_name.into(),
            payload,
        });
    }

    /// 上报插件内部模型调用产生的用量，并向前端发送用量事件。
    pub fn report_token_usage(&self, usage: tiangong_types::TokenUsage, source: impl Into<String>) {
        let _ = self.tx.send(Command::ReportUsage {
            usage,
            source: source.into(),
            emit_event: true,
        });
    }

    /// 将已经逐笔通知过前端的嵌套执行用量并入当前 turn，不重复发送用量事件。
    pub fn accumulate_token_usage(
        &self,
        usage: tiangong_types::TokenUsage,
        source: impl Into<String>,
    ) {
        let _ = self.tx.send(Command::ReportUsage {
            usage,
            source: source.into(),
            emit_event: false,
        });
    }

    /// 向当前 turn 投递一个前端流事件。
    pub fn send_stream_event(&self, event: tiangong_types::StreamEvent) {
        let _ = self.tx.send(Command::EmitStreamEvent(Box::new(event)));
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> tiangong_types::TokenUsage {
        tiangong_types::TokenUsage {
            prompt_tokens: 3,
            completion_tokens: 2,
            total_tokens: 5,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }
    }

    #[test]
    fn usage_reports_use_the_turn_command_channel() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let feedback = PluginFeedbackTx::new(tx);

        feedback.report_token_usage(usage(), "attachment");
        feedback.accumulate_token_usage(usage(), "child");

        let Command::ReportUsage {
            source, emit_event, ..
        } = rx.try_recv().unwrap()
        else {
            panic!("expected usage report");
        };
        assert_eq!(source, "attachment");
        assert!(emit_event);

        let Command::ReportUsage {
            source, emit_event, ..
        } = rx.try_recv().unwrap()
        else {
            panic!("expected accumulated usage report");
        };
        assert_eq!(source, "child");
        assert!(!emit_event);
    }
}
