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
//! [`IndexManager`] 由本插件在构造时自建并私有持有，不依赖 Tauri 句柄，可在
//! GUI / CLI / Server 全入口无条件启用。

pub mod handler;
pub mod plugin;

pub use plugin::IndexPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造索引搜索插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(IndexPlugin::new())
}

/// 构造默认的索引搜索插件列表，供各入口（CLI / Server）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
