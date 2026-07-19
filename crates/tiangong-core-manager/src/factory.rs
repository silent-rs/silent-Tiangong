//! Core 构造策略：host 注入的工厂。
//!
//! 不同 host（桌面 / 服务端 / CLI）的插件构造差异极大（桌面需要 Tauri
//! app_handle 与浏览器/终端插件，服务端挂 EventBus + ExecutionTracker），因此
//! `TiangongCore` 的实际构造由各 host 通过实现 [`CoreFactory`] 提供。
//! `CoreManager` 只负责 registry / 锁 / 配置同步 / 磁盘加载等共享逻辑。

use std::sync::mpsc::Sender;

use async_trait::async_trait;
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::CoreConfig;
use tiangong_types::StreamEvent;

/// `ensure_core` 的返回：区分新建与复用既有 Core。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsuredCore {
    pub session_id: String,
    pub is_new: bool,
}

/// TiangongCore 构造工厂。
///
/// **不接收 `Session` 参数**：session 的真相源是磁盘，Core 实例化时自行通过
/// `storage_root` + `session_id` 按需 `load_from_storage`。host 在此方法内构造
/// 自身专属的插件集合并调用 `TiangongCore::builder()...build()`。
#[async_trait]
pub trait CoreFactory: Send + Sync {
    /// 构造全新 Core。
    ///
    /// - `session_id`：会话 ID（Core 自行从磁盘 load session）
    /// - `session_config`：已按会话解析完毕的配置（含 trust_mode / reasoning_effort）
    /// - `stream_tx`：会话级事件输出通道
    async fn create(
        &self,
        session_id: &str,
        session_config: CoreConfig,
        stream_tx: Sender<StreamEvent>,
    ) -> Result<TiangongCore, String>;
}
