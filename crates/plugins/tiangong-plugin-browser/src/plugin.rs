//! 浏览器进程内插件（issue #156 自注册架构）。
//!
//! [`BrowserPlugin`] 封装浏览器的全部能力（页面获取 + 工具覆盖），在 engine
//! 创建/重建时自行注册，替代 main.rs 的手工胶水代码。

use std::sync::Arc;

use tauri::{Manager, Wry};
use tiangong_core::browser_trait::PageFetcher;
use tiangong_core::core::Plugin;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::tool_override::ToolOverrideHandler;

use crate::page_fetcher::{BrowserPageFetcher, BrowserToolOverride};

/// 浏览器插件：聚合页面获取能力与工具覆盖处理器，自行向 engine 注册。
pub struct BrowserPlugin {
    fetcher: Arc<dyn PageFetcher>,
    override_handler: Arc<dyn ToolOverrideHandler>,
}

impl BrowserPlugin {
    /// 从 Tauri 应用句柄构造浏览器插件。
    ///
    /// 复用现有的 `BrowserPageFetcher` / `BrowserToolOverride`，仅在外层包一层
    /// 「自注册」入口。返回 `None` 表示插件 state 未就绪（与旧 `get_*` 工厂一致）。
    pub fn from_app_handle(app: &tauri::AppHandle<Wry>) -> Option<Self> {
        let state = app.state::<crate::BrowserPluginState>();
        let fetcher: Arc<dyn PageFetcher> = Arc::new(BrowserPageFetcher::new(state.cmd_tx.clone()));
        let override_handler: Arc<dyn ToolOverrideHandler> =
            Arc::new(BrowserToolOverride::new(fetcher.clone()));
        Some(Self {
            fetcher,
            override_handler,
        })
    }
}

impl Plugin for BrowserPlugin {
    fn id(&self) -> &str {
        "browser"
    }

    fn register(&self, engine: &RuntimeEngine) {
        engine.set_page_fetcher(self.fetcher.clone());
        // 浏览器覆盖的 7 个工具（与旧 main.rs 手工注册清单一致）
        for tool_name in [
            "web_fetch",
            "web_browse",
            "web_form_extract",
            "web_form_fill",
            "web_click",
            "web_query_dom",
            "web_locate_element",
        ] {
            engine.register_tool_override(tool_name, self.override_handler.clone());
        }
    }
}
