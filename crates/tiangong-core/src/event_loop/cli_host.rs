//! CliLoopHost：CLI 单会话事件循环宿主

use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use crate::app_state::TurnEvent;
use crate::runtime::RuntimeEngine;
use crate::session::Session;

use super::host::{LoopHandle, LoopHost};
use super::runner::EventLoopRunner;
use super::types::*;

/// CLI 单会话事件循环宿主
pub struct CliLoopHost {
    /// 当前会话 ID
    session_id: String,
    /// 运行中的 loop 句柄
    handle: Option<LoopHandle>,
    /// 输出接收端
    output_rx: Option<Receiver<TurnEvent>>,
    /// 工作线程
    thread: Option<JoinHandle<LoopOutcome>>,
    /// 挂起的状态
    suspended_state: Option<LoopState>,
    /// 引擎
    engine: RuntimeEngine,
    /// 会话
    session: Session,
}

impl CliLoopHost {
    pub fn new(engine: RuntimeEngine, session: Session) -> Self {
        let session_id = session.id.clone();
        Self {
            session_id,
            handle: None,
            output_rx: None,
            thread: None,
            suspended_state: None,
            engine,
            session,
        }
    }

    /// 确保 loop 正在运行
    fn ensure_running(&mut self) {
        if self.handle.is_some() {
            // 检查线程是否已结束
            if self
                .thread
                .as_ref()
                .is_some_and(|t| t.is_finished())
            {
                self.collect_finished();
            } else {
                return; // 仍在运行
            }
        }

        let (event_tx, event_rx) = mpsc::channel::<LoopEvent>();
        let (output_tx, output_rx) = mpsc::channel::<TurnEvent>();

        let runner = if let Some(state) = self.suspended_state.take() {
            EventLoopRunner::resume(
                self.engine.clone(),
                self.session.clone(),
                state,
                output_tx,
                event_rx,
            )
        } else {
            EventLoopRunner::new(self.engine.clone(), self.session.clone(), output_tx, event_rx)
        };

        let thread = thread::spawn(move || runner.run());

        self.handle = Some(LoopHandle {
            session_id: self.session_id.clone(),
            event_tx,
        });
        self.output_rx = Some(output_rx);
        self.thread = Some(thread);
    }

    /// 收集已完成的线程
    fn collect_finished(&mut self) {
        if let Some(thread) = self.thread.take() {
            if thread.is_finished() {
                match thread.join() {
                    Ok(outcome) => {
                        self.suspended_state = Some(outcome.into_state());
                    }
                    Err(_) => {
                        tracing::warn!("CLI EventLoop 线程 panic");
                    }
                }
                self.handle = None;
                self.output_rx = None;
            } else {
                self.thread = Some(thread);
            }
        }
    }

    /// 切换到新会话
    pub fn switch_session(&mut self, engine: RuntimeEngine, session: Session) {
        // 先关闭当前 loop
        if let Some(ref handle) = self.handle {
            handle.send(LoopEvent::SystemSignal(SystemSignalKind::Shutdown));
        }
        self.collect_finished();

        self.session_id = session.id.clone();
        self.engine = engine;
        self.session = session;
        self.handle = None;
        self.output_rx = None;
        self.thread = None;
        self.suspended_state = None;
    }

    /// 更新会话引用（外部修改会话后同步）
    pub fn update_session(&mut self, session: Session) {
        self.session = session;
    }
}

impl LoopHost for CliLoopHost {
    fn send_event(&mut self, _session_id: &str, event: LoopEvent) {
        self.ensure_running();
        if let Some(ref handle) = self.handle {
            handle.send(event);
        }
    }

    fn output_receiver(&self, _session_id: &str) -> Option<&Receiver<TurnEvent>> {
        self.output_rx.as_ref()
    }

    fn shutdown_all(&mut self) {
        if let Some(ref handle) = self.handle {
            handle.send(LoopEvent::SystemSignal(SystemSignalKind::Shutdown));
        }
        // 等待线程退出
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.handle = None;
        self.output_rx = None;
    }

    fn is_active(&self, session_id: &str) -> bool {
        self.session_id == session_id
            && (self.handle.is_some() || self.suspended_state.is_some())
    }
}
