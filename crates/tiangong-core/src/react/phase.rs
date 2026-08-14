//! 执行阶段数据与执行预算（任务 03）。
//!
//! 阶段数据类型集中于此，供 execute.rs 及后续拆分的兄弟模块（模型/工具/审批/
//! 压缩驱动）统一使用。所有权模式（take/install）与取消方式已由任务 02 原型验证，
//! 结论见 design.md 3.1；任务 04 起在此数据模型上接入正式 `ExecutionPhase` 驱动。

use std::collections::{HashMap, HashSet, VecDeque};

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::{Id as TaskId, JoinSet};

use super::compression::ActiveCompression;
use super::outcome::TurnExecutionResult;
use crate::model::{
    InvalidToolCall, ModelFunctionResponse, ModelStreamChunk, TokenUsage, ToolCall,
};
use crate::stream_throttle::ThrottledStreamSink;
use crate::tool::ToolResult;

/// 执行阶段：任意时刻当前阶段唯一（ALR-001）。
///
/// Ready 阶段（NeedModel / PreparingTools / PendingFinish）由驱动同步推进；
/// Waiting 阶段持有活动资源并进入事件等待。
///
/// 工具/审批/压缩变体均为正式阶段：批次、任务集合、审批与批次同体持有；
/// 压缩的续接去向由 `CompressionContinuation` 完整表达（任务 06）。
/// 任务 15 起不再有独立完成度检查（Summary）或强制最终回复阶段：模型无工具
/// 调用的响应只形成候选完成，由 `contract::TaskContract` 同步门控（ALR-003）。
pub(super) enum ExecutionPhase {
    /// Ready：需要发起下一次 ReAct 模型请求。
    NeedModel,
    /// Waiting：ReAct 模型请求进行中。
    WaitingModel(ActiveLlm),
    /// Ready：大循环已产出暂定结果，待提交（任务 07 接入命令仲裁）。
    PendingFinish(TurnExecutionResult),
    /// Ready：准备/执行工具批次（逐个弹出调用，去重/审批/直接就绪分流）。
    PreparingTools(ToolBatchState),
    /// Waiting：工具任务运行中（任务集合与运行记录一一对应，不变量 3）。
    WaitingTools(ToolExecutionPhase),
    /// Waiting：等待审批（必须同时持有待审批工具与完整批次，不变量 2）。
    WaitingApproval(ApprovalPhase),
    /// Waiting：上下文压缩进行中。续接去向（return_to）由
    /// `CompressionContinuation` 完整表达：React 文本 / 工具批次（挂起批次随
    /// 阶段持有）/ 无效工具调用重生成 / 上下文超限重试。
    /// 压缩完成后只能迁移到续接允许的阶段（单一 match，不散落修改字段）。
    Compressing(CompressionPhase),
}

/// 压缩阶段数据（design.md 2.3 `CompressionPhase`）：活动压缩任务 +
/// （仅 ToolBatch 续接时）挂起的工具批次；续接去向见
/// `compression::CompressionContinuation`（即 CompressionReturn）。
pub(super) struct CompressionPhase {
    pub(super) active: ActiveCompression<super::compression::CompressionContinuation>,
    /// ToolBatch 续接时保留批次，压缩完成后回到 PreparingTools。
    pub(super) suspended_batch: Option<ToolBatchState>,
}

impl ExecutionPhase {
    /// 阶段名（迁移日志用，不含敏感内容）。
    pub(super) fn name(&self) -> &'static str {
        match self {
            ExecutionPhase::NeedModel => "NeedModel",
            ExecutionPhase::WaitingModel(_) => "WaitingModel",
            ExecutionPhase::PendingFinish(_) => "PendingFinish",
            ExecutionPhase::PreparingTools(_) => "PreparingTools",
            ExecutionPhase::WaitingTools(_) => "WaitingTools",
            ExecutionPhase::WaitingApproval(_) => "WaitingApproval",
            ExecutionPhase::Compressing(_) => "Compressing",
        }
    }
}

/// 执行预算：物理 turn 内的各阶段计数。
///
/// `reset_for_new_intent`（引导消息注入时使用）代表新的用户意图，清阶段级计数。
/// `request_round` 是物理 turn 内的日志/事件序号，任何重置都保留它；
/// `accumulated_usage` 不在此结构中（永远不重置）。
///
/// 任务 15 起不再有 `continuation_count` / `max_outer_iterations`：工具协议
/// 修复计数在 `contract::TaskContract` 中，独立且很小（ALR-305）。
#[derive(Default)]
pub(super) struct ExecutionBudget {
    /// 物理 turn 内的全局请求编号（仅日志/流事件，不参与重置决策）。
    pub(super) request_round: usize,
    /// 当前 ReAct 阶段内的轮数（达到 `max_tool_rounds` 时安全终止）。
    pub(super) react_rounds_in_phase: usize,
}

impl ExecutionBudget {
    /// 新用户意图：清阶段级计数，保留全局请求编号。
    pub(super) fn reset_for_new_intent(&mut self) {
        self.react_rounds_in_phase = 0;
    }

    /// 诊断摘要（迁移日志用，ALR-301）。
    pub(super) fn debug_summary(&self) -> String {
        format!(
            "budget: request_round={} react_rounds={}",
            self.request_round, self.react_rounds_in_phase,
        )
    }
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
/// 避免审批完成后丢失尚未处理的工具和批次元数据。
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
    React { request_injection_generation: u64 },
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
        };
        format!(
            "llm: purpose={purpose} streamed_chars={} usage_total={}",
            self.streamed_text.chars().count(),
            self.streaming_usage.total_tokens,
        )
    }
}
