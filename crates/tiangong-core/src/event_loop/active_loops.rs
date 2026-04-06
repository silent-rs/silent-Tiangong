//! ActiveLoops：多会话事件循环管理器（GUI/Server 模式）

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::app_state::TurnEvent;
use crate::runtime::RuntimeEngine;
use crate::session::Session;

use super::host::{LoopHandle, LoopHost};
use super::runner::EventLoopRunner;
use super::types::*;

type EngineFactory = Box<dyn Fn() -> RuntimeEngine + Send>;
type SessionProvider = Box<dyn Fn(&str) -> Option<Session> + Send>;

/// 运行中的 loop
struct RunningLoop {
    /// 事件发送端
    handle: LoopHandle,
    /// 输出接收端
    output_rx: Receiver<TurnEvent>,
    /// 工作线程
    thread: Option<JoinHandle<LoopOutcome>>,
}

/// 多会话事件循环管理器
pub struct MultiLoopHost {
    /// 运行中的 loop
    running: HashMap<String, RunningLoop>,
    /// 挂起的 loop（仅状态，无线程）
    suspended: HashMap<String, LoopState>,
    /// 用于创建新 loop 的 engine 工厂
    engine_factory: EngineFactory,
    /// 用于获���会话的回调
    session_provider: SessionProvider,
}

impl MultiLoopHost {
    pub fn new(
        engine_factory: impl Fn() -> RuntimeEngine + Send + 'static,
        session_provider: impl Fn(&str) -> Option<Session> + Send + 'static,
    ) -> Self {
        Self {
            running: HashMap::new(),
            suspended: HashMap::new(),
            engine_factory: Box::new(engine_factory),
            session_provider: Box::new(session_provider),
        }
    }

    /// 启动一个新 loop 或从挂起状态恢复
    fn ensure_running(&mut self, session_id: &str) -> Option<&LoopHandle> {
        if self.running.contains_key(session_id) {
            return self.running.get(session_id).map(|r| &r.handle);
        }

        let engine = (self.engine_factory)();
        let session = (self.session_provider)(session_id)?;

        let (event_tx, event_rx) = mpsc::channel::<LoopEvent>();
        let (output_tx, output_rx) = mpsc::channel::<TurnEvent>();

        let runner = if let Some(state) = self.suspended.remove(session_id) {
            EventLoopRunner::resume(engine, session, state, output_tx, event_rx)
        } else {
            EventLoopRunner::new(engine, session, output_tx, event_rx)
        };

        let thread = thread::spawn(move || runner.run());

        let handle = LoopHandle {
            session_id: session_id.to_string(),
            event_tx,
        };

        self.running.insert(
            session_id.to_string(),
            RunningLoop {
                handle: handle.clone(),
                output_rx,
                thread: Some(thread),
            },
        );

        self.running.get(session_id).map(|r| &r.handle)
    }

    /// 收集已完成的 loop，将状态转为 suspended
    pub fn collect_finished(&mut self) {
        let finished_ids: Vec<String> = self
            .running
            .iter()
            .filter(|(_, r)| {
                r.thread
                    .as_ref()
                    .is_some_and(|t| t.is_finished())
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in finished_ids {
            if let Some(mut running) = self.running.remove(&id)
                && let Some(thread) = running.thread.take()
            {
                match thread.join() {
                    Ok(outcome) => {
                        let state = outcome.into_state();
                        self.suspended.insert(id, state);
                    }
                    Err(_) => {
                        tracing::warn!("EventLoop 线程 panic");
                    }
                }
            }
        }
    }

    /// 清理长时间不活跃的挂起 loop（持久化到磁盘后移除）
    pub fn cleanup_inactive(&mut self, max_idle_secs: u64) {
        let now = chrono::Local::now().naive_local();
        let mut to_remove = Vec::new();

        for (id, state) in &self.suspended {
            if let Some(ref suspended_at) = state.suspended_at
                && let Ok(ts) = chrono::NaiveDateTime::parse_from_str(
                    suspended_at,
                    "%Y-%m-%d %H:%M:%S%.f",
                )
            {
                let elapsed = (now - ts).num_seconds().unsigned_abs();
                if elapsed > max_idle_secs {
                    if let Err(err) = super::persistence::save_loop_state(state) {
                        tracing::warn!("持久化不活跃 loop {id} 失败：{err}");
                    }
                    to_remove.push(id.clone());
                }
            }
        }

        for id in to_remove {
            self.suspended.remove(&id);
            tracing::info!("不活跃 loop {id} 已持久化并从内存移除");
        }
    }

    /// 从磁盘恢复持久化的 loop 状态到 suspended
    pub fn restore_from_disk(&mut self) {
        match super::persistence::load_all_loop_states() {
            Ok(states) => {
                for state in states {
                    let id = state.session_id.clone();
                    if !self.running.contains_key(&id) && !self.suspended.contains_key(&id) {
                        self.suspended.insert(id, state);
                    }
                }
            }
            Err(err) => {
                tracing::warn!("从磁盘恢复 LoopState 失败：{err}");
            }
        }
    }

    /// 活跃的会话数（running + suspended）
    pub fn active_count(&self) -> usize {
        self.running.len() + self.suspended.len()
    }
}

impl LoopHost for MultiLoopHost {
    fn send_event(&mut self, session_id: &str, event: LoopEvent) {
        // 先收集已完成的 loop
        self.collect_finished();

        if let Some(handle) = self.ensure_running(session_id) {
            handle.send(event);
        }
    }

    fn output_receiver(&self, session_id: &str) -> Option<&Receiver<TurnEvent>> {
        self.running.get(session_id).map(|r| &r.output_rx)
    }

    fn shutdown_all(&mut self) {
        // 向所有 running loop 发送 Shutdown 信号
        for running in self.running.values() {
            running
                .handle
                .send(LoopEvent::SystemSignal(SystemSignalKind::Shutdown));
        }

        // 等待所有线程退出（超时 5 秒）
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut ids_to_collect: Vec<String> = self.running.keys().cloned().collect();

        while !ids_to_collect.is_empty() && std::time::Instant::now() < deadline {
            ids_to_collect.retain(|id| {
                if let Some(r) = self.running.get(id) {
                    !r.thread.as_ref().is_some_and(|t| t.is_finished())
                } else {
                    false
                }
            });
            if !ids_to_collect.is_empty() {
                thread::sleep(Duration::from_millis(100));
            }
        }

        // 收集所有已完成的
        self.collect_finished();

        // 强制移除仍在运行的（超时）
        for (id, _) in self.running.drain() {
            tracing::warn!("EventLoop {id} 关闭超时，强制终止");
        }

        // 持久化所有 suspended 状态到磁盘
        for (id, state) in self.suspended.drain() {
            if let Err(err) = super::persistence::save_loop_state(&state) {
                tracing::warn!("shutdown 持久化 loop {id} 失败：{err}");
            }
        }
    }

    fn is_active(&self, session_id: &str) -> bool {
        self.running.contains_key(session_id) || self.suspended.contains_key(session_id)
    }
}
