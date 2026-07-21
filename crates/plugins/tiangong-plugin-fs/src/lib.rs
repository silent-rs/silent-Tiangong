//! 基础文件工具进程内插件。
//!
//! 把 core `LocalToolExecutor` 中的 7 个纯文件/简单工具（list_dir / tree_dir /
//! read_file / current_time / write_file / replace_in_file / apply_patch）以
//! 插件形式注入 Agent，让 core 不再感知这些工具的定义与执行。
//! （search_code 已迁至 tiangong-plugin-index 插件，与 index_search 同属检索语义。）
//!
//! 工具规格与覆盖处理器直接在 [`FsPlugin`] 上实现（supertrait 自动收集），
//! 参数全部命名化（直接读 `call.arguments` JSON），绕开旧的位置参数转换。
//!
//! 与 browser/terminal 不同，fs 不依赖 Tauri 句柄，可在 GUI / CLI / Server
//! 全入口无条件启用。

mod file_lock;
pub mod handler;
pub mod plugin;

pub use plugin::FsPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造基础文件工具插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(FsPlugin::new())
}

/// 构造默认的基础文件工具插件列表，供各入口（CLI / Server）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
