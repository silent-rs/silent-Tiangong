//! 多代理协调层类型定义

use serde::{Deserialize, Serialize};

use crate::model::TokenUsage;
use crate::session::Message;

/// 协调器任务：描述需要拆分执行的复杂任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorTask {
    /// 任务 ID
    pub id: String,
    /// 任务目标
    pub objective: String,
    /// 原始用户输入
    pub user_input: String,
    /// 上下文消息
    pub context: Vec<Message>,
}

/// Worker 上下文边界
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerContext {
    /// Worker ID
    pub worker_id: String,
    /// 子任务目标
    pub task_objective: String,
    /// 可用工具名列表（空表示使用全部）
    pub available_tools: Vec<String>,
    /// 上下文范围
    pub context_scope: ContextScope,
    /// 独立工作目录（可选）
    pub working_dir: Option<String>,
    /// 预算上限
    pub budget: WorkerBudget,
}

/// Worker 上下文范围
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScope {
    /// 完整会话上下文
    Full,
    /// 仅当前子任务上下文
    TaskOnly,
    /// 完全隔离（带初始上下文）
    Isolated { initial_context: Vec<Message> },
}

/// Worker 预算上限
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerBudget {
    /// 最大 token 数
    pub max_tokens: usize,
    /// 最大 ReAct 轮次
    pub max_rounds: usize,
    /// 最大执行时长（秒）
    pub max_duration_secs: u64,
}

impl Default for WorkerBudget {
    fn default() -> Self {
        Self {
            max_tokens: 32_768,
            max_rounds: 20,
            max_duration_secs: 300,
        }
    }
}

/// Worker 执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResult {
    /// Worker ID
    pub worker_id: String,
    /// 执行结果文本
    pub result_text: String,
    /// 是否成功
    pub success: bool,
    /// 错误信息
    pub error: Option<String>,
    /// Token 用量
    pub usage: TokenUsage,
    /// 执行时长（毫秒）
    pub duration_ms: u64,
    /// LLM 调用记录（开发调试用）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub llm_calls: Vec<crate::session::LlmCallRecord>,
}

/// 协调器最终结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorResult {
    /// 最终合并的回复
    pub final_response: String,
    /// 各 Worker 结果
    pub worker_results: Vec<WorkerResult>,
    /// 总 Token 用量
    pub total_usage: TokenUsage,
}

/// 执行环境
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEnvironment {
    /// 前台同步（当前默认）
    #[default]
    Foreground,
    /// 本地后台
    Background,
    /// 隔离环境（独立工作目录）
    Isolated { work_dir: String },
}
