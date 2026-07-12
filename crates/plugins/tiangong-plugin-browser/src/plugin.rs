//! 浏览器进程内插件（issue #156 自注册架构）。
//!
//! [`BrowserPlugin`] 封装浏览器的全部能力（页面获取 + 工具覆盖），在 engine
//! 创建/重建时自行注册，替代 main.rs 的手工胶水代码。
//!
//! 工具规格（web_fetch 等浏览器工具）与覆盖处理器直接在 [`BrowserPlugin`] 上实现，
//! core 通过 supertrait 自动收集并按工具名路由。

use std::sync::Arc;

use crate::capability::PageFetcher;
use tauri::{Manager, Wry};
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::ToolOverrideHandler;

use crate::page_fetcher::{BrowserPageFetcher, BrowserToolOverride};
use crate::watcher::BrowserWatcher;
use tiangong_core::core::plugin::PluginFeedbackTx;

/// 浏览器插件：聚合页面获取能力与工具覆盖处理器。
pub struct BrowserPlugin {
    fetcher: Arc<BrowserPageFetcher>,
    override_handler: BrowserToolOverride,
    watcher: Arc<BrowserWatcher>,
}

impl BrowserPlugin {
    /// 从 Tauri 应用句柄构造浏览器插件。
    ///
    /// 复用现有的 `BrowserPageFetcher` / `BrowserToolOverride`，仅在外层包一层
    /// 「自注册」入口。返回 `None` 表示插件 state 未就绪（与旧 `get_*` 工厂一致）。
    pub fn from_app_handle(app: &tauri::AppHandle<Wry>) -> Option<Self> {
        let state = app.state::<crate::BrowserPluginState>();
        let fetcher = Arc::new(BrowserPageFetcher::new(state.cmd_tx.clone()));
        let override_handler = BrowserToolOverride::new(fetcher.clone() as Arc<dyn PageFetcher>);
        // 创建 session-scoped watcher：随本 plugin/Core 生命周期存在，只向当前 session
        // 的 feedback channel 注入 browser_data（#225）。构造时不 spawn，待
        // set_feedback_tx 注入通道后懒启动，同一实例只启动一次。
        let watcher = Arc::new(BrowserWatcher::new(fetcher.clone() as Arc<dyn PageFetcher>));
        Some(Self {
            fetcher,
            override_handler,
            watcher,
        })
    }
}

impl Plugin for BrowserPlugin {
    fn id(&self) -> &str {
        "browser"
    }

    // register 不再注入 PageFetcher：浏览器能力是插件内部状态，
    // 由 BrowserToolOverride / watcher 直接持有 fetcher 调用（#225 能力下沉）。

    /// 注入当前 session 的 feedback 通道给 watcher：懒启动后台观察任务，
    /// observe 到的页面变化只注入到本通道（session 隔离，不跨 session 广播）。
    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        self.watcher.set_feedback_tx(tx);
    }

    /// session 就绪后注入 session_id 到 fetcher（命令带 session_id 路由），
    /// 并从持久化恢复该 session 上次的浏览器 tab（若有）。
    fn on_session_ready(&self, session: &mut tiangong_core::session::Session) {
        self.fetcher.set_session_id(&session.id);
        // session_id 就绪后启动 watcher（之前 set_feedback_tx 只存通道不启动，
        // 避免 observe 带空 session_id 污染 bootstrap/active session）
        self.watcher.start();
        // browser tab 的恢复由 get_session_tabs 命令在 hydrate 时合并 browser
        // session store（on_session_ready 时序晚于前端 hydrate，在此注入不可靠）。
    }

    fn tool_permission_overrides(
        &self,
    ) -> std::collections::BTreeMap<String, tiangong_core::permission::PermissionLevel> {
        // web_form_extract 是只读表单提取工具，声明为 Safe，避免 core 硬编码。
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert(
            "web_form_extract".to_string(),
            tiangong_core::permission::PermissionLevel::Safe,
        );
        overrides
    }
}

impl ToolOverrideHandler for BrowserPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut tiangong_core::session::Session,
        actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        ToolOverrideHandler::handle(&self.override_handler, call, session, actor_id)
    }
}

impl tiangong_core::tool_override::ToolSpecProvider for BrowserPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        // 浏览器覆盖的 6 个工具规格：这些工具的执行全部路由到本插件的 handle，
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
