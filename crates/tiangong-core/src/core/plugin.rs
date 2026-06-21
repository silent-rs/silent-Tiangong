//! 进程内插件自注册架构（issue #156）。
//!
//! 每个 [`Plugin`] 封装自己的全部能力，在 engine 创建/重建时自行向
//! [`RuntimeEngine`] 注册，消除三方中转仓库模式。
//!
//! 设计要点：
//! - [`Plugin::register`] 接收 `&RuntimeEngine`（engine 内部用 `Arc` + interior
//!   mutability，`&self` 即可修改）。
//! - 能力 trait（`PageFetcher` / `TerminalProvider` / `ToolOverrideHandler` 等）不消除，
//!   插件内部仍然使用它们，[`Plugin`] 只是在外层包了一个「自注册」入口。
//! - 新增能力类型不需要改 [`Plugin`] trait：插件在 [`Plugin::register`] 中直接调 engine 方法。
//!
//! 注意：本模块与根 `crate::plugin`（外部清单驱动插件，MCP/skill）是两套不同的机制，
//! 不要混淆。

use crate::runtime::RuntimeEngine;

/// 进程内插件：封装自己的全部能力，在 engine 创建/重建时自行注册。
///
/// 由 [`TiangongCore`](crate::core::TiangongCore) 在构造时接收一组 `Arc<dyn Plugin>`，
/// worker_loop 在 engine 首次创建（或配置变更后重建）时遍历调用 [`Plugin::register`]。
/// 这样能力注册在 worker 接收任何用户消息之前完成，根治「注册竞态窗口」。
pub trait Plugin: Send + Sync {
    /// 插件唯一标识（日志/调试用）。
    fn id(&self) -> &str;

    /// 在 engine 创建/重建时调用，插件自行注册所有能力。
    ///
    /// 插件内部调用 `engine.set_page_fetcher(...)`、`engine.register_tool_override(...)`
    /// 等方法，把持有的 `Arc<dyn ...>` 能力注入到 engine。
    fn register(&self, engine: &RuntimeEngine);
}
