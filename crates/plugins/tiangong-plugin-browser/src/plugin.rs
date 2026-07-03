//! 浏览器进程内插件（issue #156 自注册架构）。
//!
//! [`BrowserPlugin`] 封装浏览器的全部能力（页面获取 + 工具覆盖），在 engine
//! 创建/重建时自行注册，替代 main.rs 的手工胶水代码。
//!
//! 工具规格（web_fetch 等浏览器工具）与覆盖处理器直接在 [`BrowserPlugin`] 上实现，
//! core 通过 supertrait 自动收集并按工具名路由。

use std::sync::Arc;

use tauri::{Manager, Wry};
use tiangong_core::browser_trait::PageFetcher;
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::ToolOverrideHandler;

use crate::page_fetcher::{BrowserPageFetcher, BrowserToolOverride};

/// 浏览器插件：聚合页面获取能力与工具覆盖处理器。
pub struct BrowserPlugin {
    fetcher: Arc<dyn PageFetcher>,
    override_handler: BrowserToolOverride,
}

impl BrowserPlugin {
    /// 从 Tauri 应用句柄构造浏览器插件。
    ///
    /// 复用现有的 `BrowserPageFetcher` / `BrowserToolOverride`，仅在外层包一层
    /// 「自注册」入口。返回 `None` 表示插件 state 未就绪（与旧 `get_*` 工厂一致）。
    pub fn from_app_handle(app: &tauri::AppHandle<Wry>) -> Option<Self> {
        let state = app.state::<crate::BrowserPluginState>();
        let fetcher: Arc<dyn PageFetcher> = Arc::new(BrowserPageFetcher::new(state.cmd_tx.clone()));
        let override_handler = BrowserToolOverride::new(fetcher.clone());
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

    fn register(&self, engine: &tiangong_core::runtime::RuntimeEngine) {
        // 注入页面获取能力（GUI 模式下由 Tauri Plugin 提供）
        engine.set_page_fetcher(self.fetcher.clone());
        // 工具规格 / 工具覆盖 / Prompt 段落由 core 通过 supertrait 自动收集，
        // 此处仅注入 PageFetcher 能力。
    }
}

impl ToolOverrideHandler for BrowserPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &tiangong_core::session::Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        ToolOverrideHandler::handle(&self.override_handler, call, session)
    }
}

impl tiangong_core::tool_override::ToolSpecProvider for BrowserPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        // 浏览器覆盖的 7 个工具规格：这些工具的执行全部路由到本插件的 handle，
        // 必须由本插件提供 spec（core 才能按 spec.name 注册 override）。
        // 与 basic_file_function_tools 中浏览器工具的 schema 保持一致。
        use serde_json::json;
        vec![
            ToolSpec {
                name: "web_fetch".to_string(),
                description: "使用内嵌浏览器获取 URL 内容。支持 HTTP/HTTPS 网页和本地 file:// 或绝对路径的 HTML 文件。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "要获取的 HTTP/HTTPS URL" },
                        "mode": { "type": "string", "enum": ["text", "download"], "description": "执行模式，默认 text" },
                        "max_chars": { "type": "integer", "description": "text 模式最多返回字符数，默认 12000，最大 50000", "minimum": 1, "maximum": 50000 },
                        "output_path": { "type": "string", "description": "download 模式目标文件路径，必须位于允许写入目录" },
                        "overwrite": { "type": "boolean", "description": "download 模式是否覆盖已有文件，默认 false" },
                        "timeout_ms": { "type": "integer", "description": "请求超时时间，默认 15000，最大 60000", "minimum": 1000, "maximum": 60000 },
                        "follow_redirects": { "type": "boolean", "description": "是否跟随重定向，默认 true" },
                        "extract_mode": { "type": "string", "enum": ["auto", "text", "raw"], "description": "text 模式提取方式，默认 auto" }
                    },
                    "required": ["url"]
                }),
            },
            ToolSpec {
                name: "web_form_extract".to_string(),
                description: "提取内嵌浏览器当前页面中所有表单的字段信息，供 web_form_fill 填写。".to_string(),
                input_schema: json!({ "type": "object", "properties": {}, "required": [] }),
            },
            ToolSpec {
                name: "web_form_fill".to_string(),
                description: "在内嵌浏览器当前页面中填写指定表单字段。支持原生 HTML 控件和 UI 库自定义组件。selector 可传 CSS 选择器或自然语言定位描述。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string", "description": "字段定位描述。可用 CSS selector 或自然语言，如 label=邮箱、placeholder=请输入、name=email" },
                        "value": { "type": "string", "description": "要填写的值" },
                        "strategy": { "type": "string", "enum": ["auto", "native", "keyboard", "paste"], "description": "填写策略，默认 auto（自动选择最佳策略）" },
                        "wait_for": { "type": "string", "description": "填写后等待的条件（可选），如某个元素出现" }
                    },
                    "required": ["selector", "value"]
                }),
            },
            ToolSpec {
                name: "web_click".to_string(),
                description: "在内嵌浏览器当前页面中点击元素（按钮、链接等）。selector 可传 CSS 选择器或自然语言定位描述。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string", "description": "点击目标定位描述，如 登录按钮、text=提交、role=button[name=登录]" },
                        "wait_for": { "type": "string", "description": "点击后等待的条件（可选），如页面跳转或元素变化" }
                    },
                    "required": ["selector"]
                }),
            },
            ToolSpec {
                name: "web_query_dom".to_string(),
                description: "在内嵌浏览器当前页面中用 CSS 选择器查询 DOM 元素，返回匹配元素的标签、文本、属性和位置信息。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string", "description": "CSS 选择器表达式（如 .api-key-value、#result、[data-testid]）" },
                        "max_results": { "type": "integer", "description": "最大返回数量，默认 20", "minimum": 1, "maximum": 50 }
                    },
                    "required": ["selector"]
                }),
            },
            ToolSpec {
                name: "web_locate_element".to_string(),
                description: "在内嵌浏览器当前页面中定位元素，返回候选列表。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "元素定位查询" }
                    },
                    "required": ["query"]
                }),
            },
        ]
    }
}

// PromptSectionProvider 使用默认空实现（浏览器不注入 prompt 段落）
impl tiangong_core::tool_override::PromptSectionProvider for BrowserPlugin {}
