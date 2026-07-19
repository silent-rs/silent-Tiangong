//! tiangong-core-manager
//!
//! 会话级 TiangongCore 注册表与资源管理层（issue #245）。
//!
//! 定位：app-state 收窄后剥离出来的「资源加载与管理」职责的归宿。
//!
//! - **CoreManager**：持有 `session_id -> TiangongCore` 映射，提供 ensure / retire /
//!   sync_config / load_session 等管理性操作，**不执行 turn**。CoreManager 针对
//!   TiangongCore，不抽象「其他 Core 类型」——Core 构造内置在 `ensure_core`，
//!   host 在调用前构造好 plugin 集合并作为参数传入
//! - **SessionMetadata**：UI 展示 + 配置构建所需的轻量视图，替代完整 Session 列表
//!
//! 真相源仍是磁盘：CoreManager 只做按需加载与缓存，session 真正的写入仍由 Core
//! 的 turn worker 实时落盘（`PERSISTENCE_WRITE_LOCK` 保证原子写）。

pub mod core_manager;
mod metadata;
mod workspace;

pub use core_manager::{CoreManager, CoreRegistry, CoreRegistryGuard, EnsuredCore};
pub use metadata::SessionMetadata;
pub use workspace::resolve_effective_cwd;
