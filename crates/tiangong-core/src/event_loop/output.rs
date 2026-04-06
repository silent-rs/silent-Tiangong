//! EventLoop 输出回调接口
//!
//! core 内部完成完整处理链路（LLM 调用 → 工具执行 → session 更新），
//! 通过回调通知外部有新内容，外部只负责展示。

use crate::model::TokenUsage;
use crate::tool::ToolResult;

/// EventLoop 输出回调
///
/// core 在处理过程中通过此回调通知外部：
/// - CLI 实现：直接打印到终端
/// - GUI 实现：推送到前端（emit event）
pub trait LoopOutput: Send {
    /// 流式文本增量（assistant 消息内容）
    fn on_delta(&self, content: &str);

    /// 流式 reasoning 增量（思考过程）
    fn on_reasoning_delta(&self, content: &str);

    /// 工具开始执行
    fn on_tool_start(&self, name: &str, args_summary: &str);

    /// 工具执行完成
    fn on_tool_result(&self, name: &str, result: &ToolResult);

    /// LLM 调用完成（含 token 统计）
    fn on_llm_complete(&self, usage: &TokenUsage);

    /// 本轮处理完成（LLM 返回最终回复，无工具调用）
    fn on_round_complete(&self);

    /// 执行出错
    fn on_error(&self, error: &str);

    /// 需要用户审批
    fn on_approval_request(&self, request_id: &str, tool_name: &str, args_summary: &str);
}

/// 空实现（静默，不输出任何内容）
pub struct SilentOutput;

impl LoopOutput for SilentOutput {
    fn on_delta(&self, _content: &str) {}
    fn on_reasoning_delta(&self, _content: &str) {}
    fn on_tool_start(&self, _name: &str, _args_summary: &str) {}
    fn on_tool_result(&self, _name: &str, _result: &ToolResult) {}
    fn on_llm_complete(&self, _usage: &TokenUsage) {}
    fn on_round_complete(&self) {}
    fn on_error(&self, _error: &str) {}
    fn on_approval_request(&self, _request_id: &str, _tool_name: &str, _args_summary: &str) {}
}

/// Channel 适配器：将回调转为 TurnEvent 发到 channel（兼容现有 GUI poll 机制）
pub struct ChannelOutput {
    tx: std::sync::mpsc::Sender<crate::app_state::TurnEvent>,
}

impl ChannelOutput {
    pub fn new(tx: std::sync::mpsc::Sender<crate::app_state::TurnEvent>) -> Self {
        Self { tx }
    }
}

impl LoopOutput for ChannelOutput {
    fn on_delta(&self, content: &str) {
        let _ = self.tx.send(crate::app_state::TurnEvent::Chunk(
            crate::model::ModelStreamChunk {
                content: content.to_string(),
                reasoning_content: String::new(),
            },
        ));
    }

    fn on_reasoning_delta(&self, content: &str) {
        let _ = self.tx.send(crate::app_state::TurnEvent::Chunk(
            crate::model::ModelStreamChunk {
                content: String::new(),
                reasoning_content: content.to_string(),
            },
        ));
    }

    fn on_tool_start(&self, name: &str, summary: &str) {
        let _ = self.tx.send(crate::app_state::TurnEvent::ToolStarted {
            name: name.to_string(),
            summary: summary.to_string(),
        });
    }

    fn on_tool_result(&self, _name: &str, result: &ToolResult) {
        let _ = self
            .tx
            .send(crate::app_state::TurnEvent::ToolExecution(result.clone()));
    }

    fn on_llm_complete(&self, _usage: &TokenUsage) {}

    fn on_round_complete(&self) {}

    fn on_error(&self, error: &str) {
        let _ = self
            .tx
            .send(crate::app_state::TurnEvent::Failed(error.to_string()));
    }

    fn on_approval_request(&self, request_id: &str, tool_name: &str, args_summary: &str) {
        let _ = self
            .tx
            .send(crate::app_state::TurnEvent::ApprovalRequest {
                request_id: request_id.to_string(),
                tool_name: tool_name.to_string(),
                tool_args_summary: args_summary.to_string(),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_output_compiles() {
        let output = SilentOutput;
        output.on_delta("test");
        output.on_reasoning_delta("think");
        output.on_round_complete();
    }
}
