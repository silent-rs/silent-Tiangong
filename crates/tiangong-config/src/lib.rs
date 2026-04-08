//! tiangong-config：天工完整配置管理
//!
//! 定义 `TiangongConfig`（完整应用配置），提供磁盘加载/保存，
//! 并通过 `to_core_config()` 转换为 TiangongCore 所需的最小配置。
//!
//! ```text
//! TiangongConfig（应用层完整配置）
//!   ├── models      → CoreConfig.models
//!   ├── mcp         → CoreConfig.mcp
//!   ├── skills      → CoreConfig.skills
//!   ├── trust_mode  → CoreConfig.trust_mode
//!   ├── server      ← Core 不关心
//!   └── connectors  ← Core 不关心
//!              ↓ to_core_config()
//!          CoreConfig（最小契约）
//! ```
//!
//! CLI/GUI/Server 操作 TiangongConfig，TiangongCore 只接收 CoreConfig。
//! 第三方开发者可直接构造 CoreConfig，无需依赖此 crate。

mod config;
mod loader;

pub use config::{ConnectorConfig, ConnectorType, ServerConfig, TiangongConfig};
pub use loader::{load_tiangong_config, load_tiangong_config_from_dir};

// re-export core config types for convenience
pub use tiangong_core::core_config::{CoreConfig, CoreConfigBuilder, CoreConfigProvider};
