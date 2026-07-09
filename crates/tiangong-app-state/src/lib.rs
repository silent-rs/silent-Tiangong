//! 应用层状态 crate
//!
//! 从 `tiangong-core` 迁出的应用状态聚合（`TiangongState`）、磁盘持久化
//! （`AppRepository`）等。本 crate 依赖 core 的 runtime/session/model 等
//! 模块；core 不再持有应用层状态。审计日志（`audit`）与消息格式化（`formatting`）
//! 因 core 内部消费而留在 core。

pub mod app_state;
