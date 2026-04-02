//! Worker：独立执行单元
//!
//! 每个 Worker 是一个独立的 TurnRunner 实例，拥有自己的上下文边界、
//! 工具子集、工作目录和预算上限。

use std::sync::mpsc;
use std::time::Instant;

use crate::app_state::{ControlSignal, TurnEvent};
use crate::runtime::RuntimeEngine;
use crate::session::Session;
use crate::turn_runner::TurnRunner;

use super::types::{WorkerContext, WorkerResult};

/// Worker 执行单元
pub struct Worker {
    context: WorkerContext,
    engine: RuntimeEngine,
    session: Session,
    /// 可选：向主线程发送事件（用于实时展示 Worker 进度）
    event_tx: Option<mpsc::Sender<TurnEvent>>,
}

impl Worker {
    pub fn new(context: WorkerContext, engine: RuntimeEngine, session: Session) -> Self {
        Self {
            context,
            engine,
            session,
            event_tx: None,
        }
    }

    /// 设置事件发送器（用于向主线程转发 Worker 事件）
    pub fn with_event_tx(mut self, tx: mpsc::Sender<TurnEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// 执行 Worker 任务
    pub fn run(mut self) -> WorkerResult {
        let started = Instant::now();
        let worker_id = self.context.worker_id.clone();

        // 设置隔离工作目录
        if let Some(ref dir) = self.context.working_dir {
            let path = std::path::PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&path);
            self.session.cwd = dir.clone();
        }

        // 创建 channel
        let (tx, _rx) = if let Some(ref event_tx) = self.event_tx {
            // 复用主线程的事件通道
            (event_tx.clone(), None)
        } else {
            let (t, r) = mpsc::channel();
            (t, Some(r))
        };
        let (_ctrl_tx, ctrl_rx) = mpsc::channel::<ControlSignal>();

        let runner = TurnRunner::new(
            self.engine,
            self.session,
            self.context.task_objective.clone(),
            tx,
            ctrl_rx,
        );

        match runner.run() {
            Ok(exec) => WorkerResult {
                worker_id,
                result_text: exec.assistant_message,
                success: true,
                error: None,
                usage: exec.usage,
                duration_ms: started.elapsed().as_millis() as u64,
            },
            Err(err) => WorkerResult {
                worker_id,
                result_text: String::new(),
                success: false,
                error: Some(err.to_string()),
                usage: Default::default(),
                duration_ms: started.elapsed().as_millis() as u64,
            },
        }
    }
}
