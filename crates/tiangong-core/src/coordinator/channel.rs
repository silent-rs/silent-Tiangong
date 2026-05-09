//! Agent Channel 通信类型
//!
//! 定义 Worker 与外部之间的命令和事件通道类型。
//! Worker 通过 AgentCommand 接收外部控制指令（取消、审批），
//! 通过 AgentEvent 向外报告执行状态。

use serde::{Deserialize, Serialize};

/// Worker 可接收的外部命令
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentCommand {
    /// 取消当前执行
    Cancel,
    /// 响应审批请求
    Approval { request_id: String, approved: bool },
}

/// Worker 向外报告的事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    /// Worker 已启动
    Started {
        worker_id: String,
        worker_label: String,
    },
    /// Worker 输出增量文本（多 Worker 模式）
    Chunk {
        worker_id: String,
        worker_label: String,
        content: String,
    },
    /// Worker 执行完成
    Completed { worker_id: String, success: bool },
}
