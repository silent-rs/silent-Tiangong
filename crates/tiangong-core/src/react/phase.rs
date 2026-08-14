//! 执行阶段数据与执行预算（任务 03）。
//!
//! 阶段数据类型集中于此，供 execute.rs 及后续拆分的兄弟模块（模型/工具/审批/
//! 压缩驱动）统一使用。所有权模式（take/install）与取消方式已由任务 02 原型验证，
//! 结论见 design.md 3.1；任务 04 起在此数据模型上接入正式 `ExecutionPhase` 驱动。

use std::collections::{HashMap, HashSet, VecDeque};

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::{Id as TaskId, JoinSet};

use crate::model::{
    InvalidToolCall, ModelFunctionResponse, ModelStreamChunk, TokenUsage, ToolCall,
};
use crate::stream_throttle::ThrottledStreamSink;
use crate::tool::ToolResult;

/// 执行预算：物理 turn 内的各阶段计数。
///
/// `reset_react_phase` 在阶段重启（总结续作、注入重启）时清阶段级计数；
/// `reset_for_new_intent`（任务 04 接入引导消息时使用）代表新的用户意图，
/// 额外清空续作次数。`request_round` 是物理 turn 内的日志/事件序号，任何重置
/// 都保留它；`accumulated_usage` 不在此结构中（永远不重置）。
#[derive(Default)]
pub(super) struct ExecutionBudget {
    /// 物理 turn 内的全局请求编号（仅日志/流事件，不参与重置决策）。
    pub(super) request_round: usize,
    /// 当前 ReAct 阶段内的轮数。
    pub(super) react_rounds_in_phase: usize,
    /// 完成度检查要求继续的次数（原 `outer_iteration`）。
    pub(super) continuation_count: u32,
    /// 当前阶段是否已执行过工具。
    pub(super) executed_tool_in_phase: bool,
}

impl ExecutionBudget {
    /// 阶段重启：清阶段级计数，保留续作次数与全局请求编号。
    pub(super) fn reset_react_phase(&mut self) {
        self.react_rounds_in_phase = 0;
        self.executed_tool_in_phase = false;
    }

    /// 新用户意图（任务 04 引导消息接入后使用）：额外清空续作次数。
    #[allow(dead_code)]
    pub(super) fn reset_for_new_intent(&mut self) {
        self.reset_react_phase();
        self.continuation_count = 0;
    }
}

/// 执行限制。任务 07 前暂由 `TurnContext.max_tool_rounds` / `max_outer_iterations`
/// 承担，本类型先落位、接入时替换（避免控制流提前变化）。
#[allow(dead_code)]
pub(super) struct ExecutionLimits {
    pub(super) max_react_rounds: usize,
    pub(super) max_continuation_checks: u32,
}

/// 已就绪待执行的单个工具调用。
pub(super) struct PreparedToolCall {
    pub(super) index: usize,
    pub(super) call: ToolCall,
    pub(super) args_summary: String,
    pub(super) dedupe_key: String,
}

/// 一批工具调用的执行状态。
pub(super) struct ToolBatchState {
    pub(super) calls: VecDeque<(usize, ToolCall)>,
    pub(super) ready_tools: Vec<PreparedToolCall>,
    pub(super) prepared_keys: HashSet<String>,
    pub(super) invalid_tool_calls: Vec<InvalidToolCall>,
    pub(super) response_usage: TokenUsage,
    pub(super) request_injection_generation: u64,
    pub(super) needs_failure_recovery: bool,
}

/// 待审批的工具调用。
pub(super) struct PendingApproval {
    pub(super) request_id: String,
    pub(super) tool: PreparedToolCall,
}

/// 运行中的工具调用记录。
pub(super) struct RunningToolCall {
    pub(super) tool: PreparedToolCall,
    pub(super) started_at: std::time::Instant,
}

/// 工具任务输出（任务完成回传）。
pub(super) struct ToolTaskOutput {
    pub(super) result: ToolResult,
    pub(super) duration_ms: u64,
}

/// `WaitingTools` 阶段数据：任务集合与运行记录必须一一对应（不变量 3）。
/// 任务 05 接入工具阶段驱动时使用。
#[allow(dead_code)]
pub(super) struct ToolExecutionPhase {
    pub(super) tasks: JoinSet<ToolTaskOutput>,
    pub(super) running: HashMap<TaskId, RunningToolCall>,
    pub(super) batch: ToolBatchState,
}

impl ToolExecutionPhase {
    /// 不变量校验：JoinSet 任务与运行记录一一对应（debug 断言用）。
    #[allow(dead_code)]
    pub(super) fn assert_running_matches(&self) {
        debug_assert_eq!(
            self.tasks.len(),
            self.running.len(),
            "JoinSet 任务数与运行记录数必须一致"
        );
    }

    /// 诊断摘要（不含工具参数等敏感正文）。
    #[allow(dead_code)]
    pub(super) fn debug_summary(&self) -> String {
        format!(
            "tools: pending={} ready={} running={} invalid={} recovery={}",
            self.batch.calls.len(),
            self.batch.ready_tools.len(),
            self.running.len(),
            self.batch.invalid_tool_calls.len(),
            self.batch.needs_failure_recovery,
        )
    }
}

/// `WaitingApproval` 阶段数据：必须同时持有待审批工具与所属完整批次（不变量 2），
/// 避免审批完成后丢失尚未处理的工具和批次元数据。任务 05 接入审批阶段时使用。
#[allow(dead_code)]
pub(super) struct ApprovalPhase {
    pub(super) pending: PendingApproval,
    pub(super) batch: ToolBatchState,
}

impl ApprovalPhase {
    /// 诊断摘要（不含请求 ID 以外的敏感内容）。
    #[allow(dead_code)]
    pub(super) fn debug_summary(&self) -> String {
        format!(
            "approval: request={} tool={} pending_batch={}",
            self.pending.request_id,
            self.pending.tool.call.name,
            self.batch.calls.len(),
        )
    }
}

/// 模型请求的用途。
pub(super) enum LlmPurpose {
    React {
        request_injection_generation: u64,
    },
    Summary {
        iteration: u32,
        request_injection_generation: u64,
    },
    ForceFinal {
        request_injection_generation: u64,
        summary_error: Option<String>,
    },
}

/// 活跃模型请求（阶段持有）。
pub(super) struct ActiveLlm {
    pub(super) purpose: LlmPurpose,
    pub(super) pending_msg_id: String,
    pub(super) sink: ThrottledStreamSink,
    pub(super) chunk_rx: UnboundedReceiver<ModelStreamChunk>,
    pub(super) task: tokio::task::JoinHandle<anyhow::Result<ModelFunctionResponse>>,
    pub(super) streamed_text: String,
    pub(super) streamed_reasoning: String,
    pub(super) streaming_usage: TokenUsage,
}

impl ActiveLlm {
    /// 诊断摘要（不含流式正文）。
    #[allow(dead_code)]
    pub(super) fn debug_summary(&self) -> String {
        let purpose = match &self.purpose {
            LlmPurpose::React { .. } => "react".to_string(),
            LlmPurpose::Summary { iteration, .. } => {
                format!("summary(iteration={iteration})")
            }
            LlmPurpose::ForceFinal { .. } => "force_final".to_string(),
        };
        format!(
            "llm: purpose={purpose} streamed_chars={} usage_total={}",
            self.streamed_text.chars().count(),
            self.streaming_usage.total_tokens,
        )
    }
}
