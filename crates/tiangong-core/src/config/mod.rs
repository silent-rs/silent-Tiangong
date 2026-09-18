//! Core 配置归拢：Agent 运行时配置、TiangongCore 最小配置契约、模型路由配置转发。
//!
//! - [`agent`]：Agent 运行时配置（信任模式、思考强度等）
//! - [`core`]：TiangongCore 运行所需的最小配置契约（模型端点、工具、权限）
//! - [`fingerprint`]：执行配置指纹（模型 + 工具声明），用于检测需要交接的变化
//! - [`models`]：模型路由配置类型的转发层（定义在 `tiangong-llm`）

pub mod agent;
pub mod core;
pub mod fingerprint;
pub mod models;
