//! Worker：独立执行单元
//!
//! 每个 Worker 在独立 tokio 任务中执行子任务。
//! 内部直接调用 ReactEngine::execute_turn，与 TiangongCore 使用相同的执行路径。
//! 通过 AgentCommand channel 接收外部控制指令（取消、审批）。

use std::sync::mpsc::Sender as StdSender;
use std::time::Instant;

use tokio::sync::mpsc as tokio_mpsc;

use crate::agents::execution_mcp_agent::execution_function_tools;
use crate::model::ToolSpec;
use crate::runtime::{RuntimeEngine, inject_enhanced_tools};
use crate::session::{MessageRole, Session};
use tiangong_types::StreamEvent;

use super::channel::AgentCommand;
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

    /// 同步执行（桥接到 async）
    ///
    /// 保留向后兼容：创建内部 tokio runtime 运行 async 执行路径。
    /// Worker 仍然不能接收外部命令，适合简单场景。
    pub fn run(self, stream_tx: &StdSender<StreamEvent>) -> WorkerResult {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("创建 Worker tokio runtime 失败");

        // dummy channel — sync run 不接收外部命令
        let (_cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<AgentCommand>();

        rt.block_on(self.run_async(stream_tx, cmd_rx))
    }

    /// async 执行（接收 AgentCommand channel）
    ///
    /// Worker 内部将 AgentCommand 转换为 Command 传给 ReactEngine，
    /// 支持 Cancel 和 Approval 指令。
    pub async fn run_async(
        self,
        stream_tx: &StdSender<StreamEvent>,
        mut agent_cmd_rx: tokio_mpsc::UnboundedReceiver<AgentCommand>,
    ) -> WorkerResult {
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

        // 内部 StreamEvent 通道：事件转发线程加上 worker_id 标记
        let (inner_tx, inner_rx) = std::sync::mpsc::channel::<StreamEvent>();

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
                        StreamEvent::Done { .. } | StreamEvent::Error { .. } => continue,
                        other => other,
                    }
                } else {
                    event
                };
                if forward_tx.send(tagged).is_err() {
                    break;
                }
            }
        });

        // 创建 AgentCommand → Command 桥接通道
        let (cmd_tx, mut cmd_rx) = tokio_mpsc::unbounded_channel::<crate::core::Command>();

        // 桥接任务：将 AgentCommand 转换为 Command 转发给 ReactEngine
        let bridge_handle = tokio::spawn(async move {
            while let Some(agent_cmd) = agent_cmd_rx.recv().await {
                let cmd = match agent_cmd {
                    AgentCommand::Cancel => crate::core::Command::Cancel,
                    AgentCommand::Approval {
                        request_id,
                        approved,
                    } => crate::core::Command::Approval {
                        request_id,
                        approved,
                    },
                };
                if cmd_tx.send(cmd).is_err() {
                    break;
                }
            }
        });

        // 准备 Worker 会话
        let mut worker_session = self.session;
        worker_session.append_message(MessageRole::User, self.context.task_objective.clone());

        // 使用 ReactEngine 直接执行（async 路径）
        let react = crate::react::engine::ReactEngine::new(
            self.engine.clone(),
            tools,
            mcp_targets,
            budget.max_rounds,
        );
        let accumulated_usage = react
            .execute_turn(
                &mut worker_session,
                &self.context.task_objective,
                &inner_tx,
                &mut cmd_rx,
                None,
            )
            .await;

        // 关闭通道，等待桥接和转发线程结束
        drop(cmd_rx);
        let _ = bridge_handle.await;
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

    /// 准备 Worker 的命令通道对
    ///
    /// 返回 (agent_cmd_tx, agent_cmd_rx) 供外部控制和 Worker 内部使用。
    pub fn command_channel() -> (
        tokio_mpsc::UnboundedSender<AgentCommand>,
        tokio_mpsc::UnboundedReceiver<AgentCommand>,
    ) {
        tokio_mpsc::unbounded_channel::<AgentCommand>()
    }
}
