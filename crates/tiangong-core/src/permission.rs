//! 权限层
//!
//! Core 保存并向插件传递当前信任模式，不内置用户审批流程。

/// 信任模式
///
/// 定义已下沉至 [`tiangong_types::TrustMode`]，此处 re-export 保持
/// `tiangong_core::permission::TrustMode` 路径稳定。
pub use tiangong_types::TrustMode;
