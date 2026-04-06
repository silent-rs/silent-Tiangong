//! LoopHost trait：统一的事件循环宿主接口
//!
//! CLI 和 GUI/Server 通过统一接口与 EventLoop 交互。

use std::sync::mpsc::Sender;

use crate::app_state::TurnEvent;

use super::types::LoopEvent;

/// 事件循环宿主接口
///
/// 无论 CLI 单会话还是 GUI 多会话，外部都通过此接口与 EventLoop 交互。
pub trait LoopHost {
    /// 向会话发送事件（自动唤起挂起的 loop）
    fn send_event(&mut self, session_id: &str, event: LoopEvent);

    /// 获取输出事件的接收端（用于 poll 输出）
    ///
    /// 返回 None 表示该会话无活跃 loop。
    fn output_receiver(&self, session_id: &str) -> Option<&std::sync::mpsc::Receiver<TurnEvent>>;

    /// 优雅关闭所有 loop
    fn shutdown_all(&mut self);

    /// 检查会话是否有活跃的 loop（Running 或 Suspended）
    fn is_active(&self, session_id: &str) -> bool;
}

/// 用于向 EventLoopRunner 发送事件的句柄
#[derive(Debug, Clone)]
pub struct LoopHandle {
    pub session_id: String,
    pub event_tx: Sender<LoopEvent>,
}

impl LoopHandle {
    pub fn send(&self, event: LoopEvent) {
        let _ = self.event_tx.send(event);
    }
}
