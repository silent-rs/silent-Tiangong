//! 统一运行时事件模型
//!
//! 所有输入和状态变化都统一转为 `RuntimeEvent`，为事件驱动状态机提供基础。

use serde::{Deserialize, Serialize};

/// 统一运行时事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEvent {
    /// 事件唯一 ID
    pub event_id: String,
    /// 事件类型
    pub event_type: RuntimeEventType,
    /// 关联的会话 ID
    pub session_id: String,
    /// 关联的任务 ID（可选）
    pub task_id: Option<String>,
    /// 事件来源
    pub source: EventSource,
    /// 事件载荷（具体数据）
    pub payload: serde_json::Value,
    /// 事件创建时间
    pub created_at: String,
}

/// 事件类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventType {
    /// 用户消息输入
    UserMessage,
    /// 工具执行结果
    ToolResult,
    /// 权限审批请求
    PermissionRequest,
    /// 权限审批响应
    PermissionResponse,
    /// 任务开始
    TaskStarted,
    /// 任务完成
    TaskCompleted,
    /// 任务失败
    TaskFailed,
    /// LLM 流式输出片段
    LlmChunk,
    /// LLM 完整输出
    LlmOutput,
    /// 通知（后台任务完成等）
    Notification,
    /// 系统信号（关闭、配置变更等）
    SystemSignal,
}

/// 事件来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// 用户交互
    User,
    /// 运行时内部
    Runtime,
    /// 工具执行
    Tool,
    /// 权限系统
    Permission,
    /// 系统级事件
    System,
    /// 远程接入（Connector）
    Connector,
}

impl RuntimeEvent {
    /// 创建新的运行时事件
    pub fn new(
        event_type: RuntimeEventType,
        session_id: String,
        source: EventSource,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_id: scru128::new().to_string(),
            event_type,
            session_id,
            task_id: None,
            source,
            payload,
            created_at: chrono::Local::now().naive_local().to_string(),
        }
    }

    /// 设置关联的任务 ID
    pub fn with_task_id(mut self, task_id: String) -> Self {
        self.task_id = Some(task_id);
        self
    }
}
