//! 查询编排层
//!
//! 整个智能体的控制中心，负责：
//! - 接受当前事件，判断是否打断正在运行的任务
//! - 裁剪或恢复会话状态
//! - 路由到不同执行模式（直接回答/单工具/多步/多代理/后台）
//! - 决定是否发起模型调用

pub mod query_orchestrator;
pub mod types;

pub use query_orchestrator::QueryOrchestrator;
pub use types::QueryMode;
