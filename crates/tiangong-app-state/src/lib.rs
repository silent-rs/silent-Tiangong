//! 应用层状态 crate
//!
//! 从 `tiangong-core` 迁出的应用状态聚合（`TiangongState`）、磁盘持久化
//! （`AppRepository`）、审计日志（`audit`）等。core 反向依赖本 crate，
//! 不再持有应用层状态。

pub mod app_state;
