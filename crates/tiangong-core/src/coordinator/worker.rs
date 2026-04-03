//! Worker：独立执行单元
//!
//! 每个 Worker 是一个独立的 TurnRunner 实例。
//! 多 Worker 模式下，Worker 拦截 Chunk 事件转为 WorkerChunk，
//! 并实时转发到主 event_tx，实现并行流式输出。

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
    event_tx: Option<mpsc::Sender<TurnEvent>>,
    ctrl_rx: Option<mpsc::Receiver<ControlSignal>>,
    /// 是否为多 Worker 模式（需要将 Chunk 转为 WorkerChunk）
    multi_worker_mode: bool,
}

impl Worker {
    pub fn new(context: WorkerContext, engine: RuntimeEngine, session: Session) -> Self {
        Self {
            context,
            engine,
            session,
            event_tx: None,
            ctrl_rx: None,
            multi_worker_mode: false,
        }
    }

    pub fn with_event_tx(mut self, tx: mpsc::Sender<TurnEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    pub fn with_ctrl_rx(mut self, rx: mpsc::Receiver<ControlSignal>) -> Self {
        self.ctrl_rx = Some(rx);
        self
    }

    /// 设置多 Worker 模式（Chunk 事件转为 WorkerChunk，实时转发）
    pub fn with_multi_worker_mode(mut self) -> Self {
        self.multi_worker_mode = true;
        self
    }

    /// 执行 Worker 任务
    pub fn run(mut self) -> WorkerResult {
        let started = Instant::now();
        let worker_id = self.context.worker_id.clone();
        let worker_label: String = self.context.task_objective.chars().take(30).collect();

        // 设置隔离工作目录
        if let Some(ref dir) = self.context.working_dir {
            let path = std::path::PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&path);
            self.session.cwd = dir.clone();
        }

        // 创建 TurnRunner 使用的 channel
        let (runner_tx, runner_rx) = mpsc::channel::<TurnEvent>();
        let ctrl_rx = self.ctrl_rx.unwrap_or_else(|| {
            let (_t, r) = mpsc::channel::<ControlSignal>();
            r
        });

        let forward_handle = if self.multi_worker_mode {
            // 多 Worker 模式：实时转发，Chunk→WorkerChunk，其他→WorkerEvent
            let main_tx = self.event_tx.take();
            let wid = worker_id.clone();
            let wlabel = worker_label.clone();

            // 发送 WorkerStarted 事件
            if let Some(ref tx) = main_tx {
                let _ = tx.send(TurnEvent::WorkerStarted {
                    worker_id: wid.clone(),
                    worker_label: wlabel.clone(),
                });
            }

            main_tx.map(|tx| {
                std::thread::spawn(move || {
                    while let Ok(event) = runner_rx.recv() {
                        let tagged = match event {
                            TurnEvent::Chunk(delta) => TurnEvent::WorkerChunk {
                                worker_id: wid.clone(),
                                worker_label: wlabel.clone(),
                                delta,
                            },
                            other => TurnEvent::WorkerEvent {
                                worker_id: wid.clone(),
                                worker_label: wlabel.clone(),
                                inner: Box::new(other),
                            },
                        };
                        if tx.send(tagged).is_err() {
                            break;
                        }
                    }
                })
            })
        } else {
            // 单 Worker 模式：实时转发，不做事件转换
            self.event_tx.take().map(|main_tx| {
                std::thread::spawn(move || {
                    while let Ok(event) = runner_rx.recv() {
                        if main_tx.send(event).is_err() {
                            break;
                        }
                    }
                })
            })
        };

        let runner = TurnRunner::new(
            self.engine,
            self.session,
            self.context.task_objective.clone(),
            runner_tx,
            ctrl_rx,
        );

        let result = match runner.run() {
            Ok(exec) => WorkerResult {
                worker_id: worker_id.clone(),
                result_text: exec.assistant_message,
                success: true,
                error: None,
                usage: exec.usage,
                duration_ms: started.elapsed().as_millis() as u64,
            },
            Err(err) => WorkerResult {
                worker_id: worker_id.clone(),
                result_text: String::new(),
                success: false,
                error: Some(err.to_string()),
                usage: Default::default(),
                duration_ms: started.elapsed().as_millis() as u64,
            },
        };

        // 等待转发线程结束
        if let Some(handle) = forward_handle {
            let _ = handle.join();
        }

        // 多 Worker 模式下发送 WorkerCompleted（通过检查 multi_worker_mode 标志已消费）
        // WorkerCompleted 由 Coordinator 在所有 Worker 完成后不再需要，
        // 因为 Worker 自己会在转发线程关闭时隐式完成

        result
    }
}
