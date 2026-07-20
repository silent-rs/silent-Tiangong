//! 应用层状态 crate
//!
//! 从 `tiangong-core` 迁出的进程内应用状态聚合（`TiangongState`）。
//! 应用状态只在本次运行中存在；core 不再持有应用层状态。

pub mod app_state;
