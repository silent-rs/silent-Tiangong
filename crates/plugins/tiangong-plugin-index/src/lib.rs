//! 索引搜索进程内插件。
//!
//! 收敛两类检索工具：
//! - `index_search`：基于 [`IndexManager`]（tantivy 全文索引）的语义检索，覆盖工作区
//!   文件内容与对话历史。原为 core 硬编码特判，现作为插件工具暴露。
//! - `search_code`：基于 `rg`/`grep` 子进程的精确文本检索。原属 fs 插件，因与
//!   `index_search` 同属「检索」语义且 description 互相引用，一并迁入本插件。
//!
//! 同时，本插件通过生命周期钩子（[`Plugin::on_turn_finished`] / [`Plugin::on_cwd_changed`]
//! / [`Plugin::on_session_ready`] / [`Plugin::on_session_ended`]）接管原 core 对
//! `IndexManager` 的全部写入与维护（初始扫描、CWD 重扫、turn 结束批量写入对话索引、
//! 会话结束 finalize），使 core 彻底不再感知 `IndexManager`。
//!
//! [`IndexManager`] 推荐由入口在初始化时构造为 app 层单例（[`shared_index_manager`]），
//! 经 [`build_plugin_with_manager`] / [`default_plugins_with_manager`] 注入各 Core 的插件
//! 列表，使所有对话共享同一底层索引缓存与扫描状态；不依赖 Tauri 句柄，可在
//! GUI / CLI / Server 全入口无条件启用。

pub mod handler;
pub mod index;
pub mod plugin;

pub use index::{
    IndexHit, IndexManager, IndexMeta, IndexQuery, IndexScope, SessionIndexHit, TurnData,
    WorkspaceIndexInfo, backfill_session_index, delete_workspace_index_for_gui,
    list_workspace_indexes_for_gui, session_index_exists, workspace_index_exists,
};
pub use plugin::IndexPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造索引搜索插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
///
/// 内部自建 IndexManager（非共享）。生产入口应优先使用
/// [`build_plugin_with_manager`] 注入 app 层单例。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(IndexPlugin::new())
}

/// 构造索引搜索插件实例，注入共享 IndexManager（app 层单例）。
pub fn build_plugin_with_manager(manager: Arc<IndexManager>) -> Arc<dyn Plugin> {
    Arc::new(IndexPlugin::from_index_manager(Some(manager)))
}

/// 构造默认的索引搜索插件列表，供各入口（CLI / Server）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}

/// 构造默认的索引搜索插件列表，注入共享 IndexManager（app 层单例）。
pub fn default_plugins_with_manager(manager: Arc<IndexManager>) -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin_with_manager(manager)]
}

/// 构造 app 层共享的 IndexManager 单例句柄，供入口在初始化时创建一次后注入各 Core。
///
/// 失败时返回 Err（调用方决定降级策略：缺省省略 index 插件或自建兜底）。
pub fn shared_index_manager() -> anyhow::Result<Arc<IndexManager>> {
    IndexManager::new().map(Arc::new)
}
