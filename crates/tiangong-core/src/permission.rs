//! 权限层
//!
//! 信任模式驱动:FullTrust 放行一切,Supervised 下所有工具调用需用户审批。
//! 审批在 turn 层统一完成,插件 handler 无需感知权限。

/// 信任模式
///
/// 定义已下沉至 [`tiangong_types::TrustMode`]，此处 re-export 保持
/// `tiangong_core::permission::TrustMode` 路径稳定。
pub use tiangong_types::TrustMode;
