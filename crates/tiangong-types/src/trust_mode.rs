//! 信任模式
//!
//! 纯数据枚举，不包含任何业务逻辑。由 core（PermissionGate / Session /
//! RuntimeEngine / Plugin）与 config（TiangongConfig）共同消费。
//!
//! 定义下沉至 `tiangong-types`，使配置层无需反向依赖 `tiangong-core`
//! 即可引用信任模式；core 经 re-export 保持 `tiangong_core::permission::TrustMode`
//! 路径稳定。

use serde::{Deserialize, Serialize};

/// 信任模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustMode {
    /// 完全信任：所有工具自动放行，不弹审批
    FullTrust,
    /// 监督模式：高风险操作需要用户确认
    #[default]
    Supervised,
}
