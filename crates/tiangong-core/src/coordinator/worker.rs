//! Worker：独立执行单元
//!
//! 每个 Worker 在独立线程中执行子任务。
//! 内部直接调用 core::execute_turn，与 TiangongCore 使用相同的执行路径。
//! 多 Worker 模式下，Worker 将 StreamEvent 转为带 worker_id 标记的事件。

use std::sync::mpsc::{self, Sender};
use std::time::Instant;

use crate::agents::execution_mcp_agent::execution_function_tools;
use crate::model::ToolSpec;
use crate::runtime::{RuntimeEngine, inject_enhanced_tools};
use crate::session::{MessageRole, Session};
use tiangong_types::StreamEvent;

use super::types::{WorkerContext, WorkerResult};

/// Worker 执行单元
pub struct Worker {
    context: WorkerContext,
    engine: RuntimeEngine,
    session: Session,
    /// 多 Worker 模式（转换事件标记 worker_id）
    multi_worker_mode: bool,
}

impl Worker {
    pub fn new(context: WorkerContext, engine: RuntimeEngine, session: Session) -> Self {
        Self {
            context,
            engine,
            session,
            multi_worker_mode: false,
        }
    }

    /// 设置多 Worker 模式
    pub fn with_multi_worker_mode(mut self) -> Self {
        self.multi_worker_mode = true;
        self
    }

    /// 执行 Worker 任务，结果通过 stream_tx 实时推送
    pub fn run(self, stream_tx: &Sender<StreamEvent>) -> WorkerResult {
        let started = Instant::now();
        let worker_id = self.context.worker_id.clone();
        let worker_label: String = self.context.task_objective.chars().take(30).collect();
        let budget = self.context.budget.clone();

        // 设置隔离工作目录
        if let Some(ref dir) = self.context.working_dir {
            let path = std::path::PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&path);
        }

        // 初始化工具
        let (all_tools, mcp_targets) = execution_function_tools(&self.engine.agent_config().mcp);
        let mut tools: Vec<ToolSpec> = all_tools
            .into_iter()
            .filter(|t| t.name != "mark_step_completed")
            .collect();
        inject_enhanced_tools(&mut tools, &self.engine);

        // 创建 Worker 内部的 stream channel
        // 如果是多 Worker 模式，拦截事件并加上 worker_id 标记
        let (inner_tx, inner_rx) = mpsc::channel::<StreamEvent>();

        // 发送 WorkerStarted 事件
        if self.multi_worker_mode {
            let _ = stream_tx.send(StreamEvent::WorkerStarted {
                worker_id: worker_id.clone(),
                worker_label: worker_label.clone(),
            });
        }

        // 事件转发线程：拦截 inner_tx 的事件，转为带标记的事件
        let forward_tx = stream_tx.clone();
        let fwd_worker_id = worker_id.clone();
        let fwd_worker_label = worker_label.clone();
        let is_multi = self.multi_worker_mode;
        let forward_handle = std::thread::spawn(move || {
            while let Ok(event) = inner_rx.recv() {
                let tagged = if is_multi {
                    match event {
                        StreamEvent::Delta { content, .. } => StreamEvent::WorkerChunk {
                            worker_id: fwd_worker_id.clone(),
                            worker_label: fwd_worker_label.clone(),
                            content,
                        },
                        StreamEvent::Done { .. } | StreamEvent::Error { .. } => {
                            continue;
                        }
                        other => other, // 其他事件透传（ToolStart/ToolResult 等）
                    }
                } else {
                    event
                };
                if forward_tx.send(tagged).is_err() {
                    break;
                }
            }
        });

        // 创建一个 dummy cmd_rx（Worker 不接收外部命令）
        let (_dummy_cmd_tx, dummy_cmd_rx) = mpsc::channel::<crate::core::Command>();

        // 调用 execute_turn（与 TiangongCore 相同的执行路径）
        let mut worker_session = self.session;
        worker_session.append_message(MessageRole::User, self.context.task_objective.clone());

        let accumulated_usage = crate::core::execute_turn_standalone(
            &mut worker_session,
            &self.context.task_objective,
            &self.engine,
            &tools,
            &mcp_targets,
            &inner_tx,
            dummy_cmd_rx,
            budget.max_rounds,
        );

        // 关闭 inner_tx，让转发线程结束
        drop(inner_tx);
        let _ = forward_handle.join();

        // 预算检查
        if budget.is_token_exceeded(accumulated_usage.total_tokens) {
            tracing::warn!(
                worker_id = %worker_id,
                used = accumulated_usage.total_tokens,
                limit = budget.max_tokens,
                "Worker token 超出预算"
            );
        }

        // 提取最后一条 assistant 消息作为结果
        let result_text = worker_session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .map(|m| m.content.clone())
            .unwrap_or_default();

        // 发送 WorkerCompleted
        if self.multi_worker_mode {
            let _ = stream_tx.send(StreamEvent::WorkerCompleted {
                worker_id: worker_id.clone(),
                worker_label: worker_label.clone(),
                success: true,
            });
        }

        WorkerResult {
            worker_id,
            result_text,
            success: true,
            error: None,
            usage: accumulated_usage,
            duration_ms: started.elapsed().as_millis() as u64,
            llm_calls: Vec::new(),
        }
    }
}
