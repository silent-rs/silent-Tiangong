//! 事件驱动状态机：包装 RuntimeEngine 执行链路
//!
//! `TurnRunner` 将 `RuntimeEngine::execute_turn_with_streaming` 包装为
//! 基于 `TurnPhase` 状态机的执行模型，通过 `mpsc::Sender<TurnEvent>` 发送事件。
//!
//! 当前阶段：TurnRunner 委托给 RuntimeEngine 执行，并在入口/出口
//! 发布 TurnPhase 状态变更。后续逐步将 for 循环拆解为状态驱动。

use std::sync::mpsc::Sender;

use anyhow::Result;

use crate::app_state::{ManagementCommand, TurnEvent};
use crate::model::ModelStreamChunk;
use crate::runtime::{LlmOutputRecord, RuntimeEngine, TurnExecution};
use crate::session::Session;
use crate::tool::ToolResult;
use crate::turn_state::TurnPhase;

/// 状态机驱动的 Turn 执行器
pub struct TurnRunner {
    engine: RuntimeEngine,
    session: Session,
    user_input: String,
    tx: Sender<TurnEvent>,
    phase: TurnPhase,
}

impl TurnRunner {
    /// 创建新的 TurnRunner
    pub fn new(
        engine: RuntimeEngine,
        session: Session,
        user_input: String,
        tx: Sender<TurnEvent>,
    ) -> Self {
        Self {
            engine,
            session,
            user_input,
            tx,
            phase: TurnPhase::Init,
        }
    }

    /// 获取当前阶段
    pub fn phase(&self) -> &TurnPhase {
        &self.phase
    }

    /// 运行状态机直到终止，返回执行结果
    pub fn run(mut self) -> Result<TurnExecution> {
        self.phase = TurnPhase::ContextAssembly;

        // 委托给 RuntimeEngine 的现有 ReAct 循环执行
        // 通过回调将事件发送到 TurnEvent channel
        let chunk_tx = self.tx.clone();
        let llm_tx = self.tx.clone();
        let tool_tx = self.tx.clone();
        let plan_tx = self.tx.clone();
        let plan_summary_tx = self.tx.clone();
        let stage_thinking_tx = self.tx.clone();
        let mgmt_tx = self.tx.clone();

        self.phase = TurnPhase::LlmCalling;

        let result = self.engine.execute_turn_with_streaming(
            &self.session,
            &self.user_input,
            |plan| {
                let _ = plan_tx.send(TurnEvent::PlanReady(plan.clone()));
            },
            |delta: &ModelStreamChunk| {
                let _ = chunk_tx.send(TurnEvent::Chunk(delta.clone()));
            },
            |output: &LlmOutputRecord| {
                let _ = llm_tx.send(TurnEvent::LlmOutput(output.clone()));
            },
            |tool_result: &ToolResult| {
                let _ = tool_tx.send(TurnEvent::ToolExecution(tool_result.clone()));
            },
            |summary: &str| {
                let _ =
                    plan_summary_tx.send(TurnEvent::PlanExecutionSummary(summary.to_string()));
            },
            |stage: &str, delta: &ModelStreamChunk| {
                let _ = stage_thinking_tx.send(TurnEvent::StageThinking {
                    stage: stage.to_string(),
                    delta: delta.clone(),
                });
            },
            |cmd: ManagementCommand| {
                let _ = mgmt_tx.send(TurnEvent::ManagementCommand(cmd));
            },
        );

        match result {
            Ok(exec) => {
                self.phase = TurnPhase::Completed;
                Ok(exec)
            }
            Err(err) => {
                self.phase = TurnPhase::Failed {
                    error: err.to_string(),
                };
                Err(err)
            }
        }
    }
}
