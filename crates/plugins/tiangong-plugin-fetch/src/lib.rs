//! web_fetch 进程内插件（基础 URL 获取能力）。
//!
//! 提供 HTTP/HTTPS 网页获取（text 模式提取正文 / download 模式落盘），含 SSRF 防护。
//! 供 CLI / Server 入口使用；GUI 入口使用 browser 插件（内嵌浏览器渲染）。
//!
//! 与 browser 插件走不同流程：本插件直接返回页面正文（reqwest blocking），
//! browser 插件通过 Tauri command 经前端浏览器执行并异步推送页面内容。

pub mod handler;
pub mod plugin;

pub use plugin::FetchPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造 web_fetch 插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(FetchPlugin::new())
}

/// 构造默认的插件列表，供 CLI / Server 入口注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
