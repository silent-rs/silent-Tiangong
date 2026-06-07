use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::types::BrowserCommand;
use tiangong_core::browser_trait::PageFetcher;

/// 通过 BrowserCommand channel 实现 PageFetcher trait
pub struct BrowserPageFetcher {
    cmd_tx: mpsc::Sender<BrowserCommand>,
}

impl BrowserPageFetcher {
    pub fn new(cmd_tx: mpsc::Sender<BrowserCommand>) -> Self {
        Self { cmd_tx }
    }

    pub fn cmd_tx(&self) -> mpsc::Sender<BrowserCommand> {
        self.cmd_tx.clone()
    }
}

macro_rules! send_and_wait {
    ($tx:expr, $cmd:expr, $rx:expr, $timeout:expr) => {{
        if $tx.send($cmd).await.is_err() {
            return None;
        }
        tokio::time::timeout(Duration::from_secs($timeout), $rx)
            .await
            .ok()?
            .ok()?
    }};
}

impl PageFetcher for BrowserPageFetcher {
    fn fetch_page(
        &self,
        url: &str,
        max_chars: usize,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<tiangong_core::browser_trait::FetchResult>>
                + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let resp: crate::types::BrowserResponse = send_and_wait!(
                tx,
                BrowserCommand::FetchPage {
                    url,
                    max_chars,
                    response_tx
                },
                response_rx,
                30
            );
            Some(resp.into())
        })
    }

    fn observe_page(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<tiangong_core::browser_trait::PageSnapshot>>
                + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let snapshot: crate::types::BrowserPageSnapshot = send_and_wait!(
                tx,
                BrowserCommand::ObservePage { response_tx },
                response_rx,
                10
            );
            Some(tiangong_core::browser_trait::PageSnapshot {
                title: snapshot.title,
                url: snapshot.url,
                text: snapshot.text,
                tabs: snapshot.tabs.into_iter().map(Into::into).collect(),
                active_tab_id: snapshot.active_tab_id,
            })
        })
    }

    fn list_tabs(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<tiangong_core::browser_trait::TabListResult>>
                + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let tabs: Vec<crate::types::BrowserTab> =
                send_and_wait!(tx, BrowserCommand::TabList { response_tx }, response_rx, 10);
            Some(tiangong_core::browser_trait::TabListResult {
                tabs: tabs.into_iter().map(Into::into).collect(),
                active_tab_id: None,
            })
        })
    }

    fn form_extract(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<tiangong_core::browser_trait::FormExtractResult>,
                > + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let result: crate::types::FormExtractResult = send_and_wait!(
                tx,
                BrowserCommand::FormExtract { response_tx },
                response_rx,
                10
            );
            Some(result.into())
        })
    }

    fn form_fill(
        &self,
        selector: &str,
        value: &str,
        strategy: &str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<tiangong_core::browser_trait::FillFieldResult>>
                + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        let selector = selector.to_string();
        let value = value.to_string();
        let strategy = strategy.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let result: crate::types::FillFieldResult = send_and_wait!(
                tx,
                BrowserCommand::FormFill {
                    selector,
                    value,
                    strategy,
                    response_tx,
                },
                response_rx,
                10
            );
            Some(result.into())
        })
    }

    fn click_element(
        &self,
        selector: &str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<tiangong_core::browser_trait::ClickElementResult>,
                > + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        let selector = selector.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let result: crate::types::ClickElementResult = send_and_wait!(
                tx,
                BrowserCommand::ClickElement {
                    selector,
                    response_tx
                },
                response_rx,
                10
            );
            Some(result.into())
        })
    }

    fn load_html(
        &self,
        html: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<std::result::Result<(), String>>> + Send>,
    > {
        let tx = self.cmd_tx.clone();
        let html = html.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            if tx
                .send(BrowserCommand::LoadHtml { html, response_tx })
                .await
                .is_err()
            {
                return None;
            }
            tokio::time::timeout(Duration::from_secs(10), response_rx)
                .await
                .ok()
                .and_then(|r| r.ok())
        })
    }
}

/// 浏览器工具覆盖处理器（web_fetch / web_form_extract / web_form_fill / web_click / web_load_html）
pub struct BrowserToolOverride {
    fetcher: Arc<dyn PageFetcher>,
}

impl BrowserToolOverride {
    pub fn new(fetcher: Arc<dyn PageFetcher>) -> Self {
        Self { fetcher }
    }

    fn handle_web_fetch(
        fetcher: &Arc<dyn PageFetcher>,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<tiangong_core::tool::ToolResult>> + Send>,
    > {
        let mode = call
            .arguments
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("text");
        let url = match call.arguments.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return Box::pin(async { None }),
        };

        // 本地路径统一转为 file:// URL，与 HTTP 走同一条浏览器打开路径
        let url = if url.starts_with('/') {
            format!("file://{url}")
        } else {
            url
        };

        if mode != "text" {
            return Box::pin(async { None });
        }
        let max_chars = call
            .arguments
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(12000) as usize;

        let fetcher = fetcher.clone();
        Box::pin(async move {
            let result = match fetcher.fetch_page(&url, max_chars).await {
                Some(r) => r,
                None => {
                    return Some(tiangong_core::tool::ToolResult {
                        ok: false,
                        summary: "浏览器获取页面失败".to_string(),
                        stdout: String::new(),
                        stderr: "浏览器无法获取页面内容".to_string(),
                        exit_code: 1,
                        execution: None,
                    });
                }
            };
            let summary = if result.ok {
                format!("浏览器获取成功：{}", result.title)
            } else {
                format!(
                    "浏览器获取失败：{}",
                    result.error.clone().unwrap_or_default()
                )
            };
            Some(tiangong_core::tool::ToolResult {
                ok: result.ok,
                summary,
                stdout: if result.ok {
                    result.content
                } else {
                    String::new()
                },
                stderr: if result.ok {
                    String::new()
                } else {
                    result.error.unwrap_or_default()
                },
                exit_code: if result.ok { 0 } else { 1 },
                execution: None,
            })
        })
    }

    fn handle_web_form_extract(
        fetcher: &Arc<dyn PageFetcher>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<tiangong_core::tool::ToolResult>> + Send>,
    > {
        let fetcher = fetcher.clone();
        Box::pin(async move {
            let result = match fetcher.form_extract().await {
                Some(r) => r,
                None => {
                    return Some(tiangong_core::tool::ToolResult {
                        ok: false,
                        summary: "浏览器未打开，无法提取表单".to_string(),
                        stdout: String::new(),
                        stderr: "请先使用 web_fetch 打开页面".to_string(),
                        exit_code: 1,
                        execution: None,
                    });
                }
            };
            let total_fields: usize = result.forms.iter().map(|f| f.fields.len()).sum();
            let output = serde_json::to_string_pretty(&result)
                .unwrap_or_else(|_| "序列化表单数据失败".to_string());
            Some(tiangong_core::tool::ToolResult {
                ok: true,
                summary: format!(
                    "提取到 {} 个表单，共 {} 个字段",
                    result.forms.len(),
                    total_fields
                ),
                stdout: output,
                stderr: String::new(),
                exit_code: 0,
                execution: None,
            })
        })
    }

    fn handle_web_form_fill(
        fetcher: &Arc<dyn PageFetcher>,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<tiangong_core::tool::ToolResult>> + Send>,
    > {
        let selector = call
            .arguments
            .get("selector")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let value = call
            .arguments
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let strategy = call
            .arguments
            .get("strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("auto")
            .to_string();
        let fetcher = fetcher.clone();
        Box::pin(async move {
            let result = match fetcher.form_fill(&selector, &value, &strategy).await {
                Some(r) => r,
                None => {
                    return Some(tiangong_core::tool::ToolResult {
                        ok: false,
                        summary: "浏览器未打开，无法填写字段".to_string(),
                        stdout: String::new(),
                        stderr: "请先使用 web_fetch 打开页面".to_string(),
                        exit_code: 1,
                        execution: None,
                    });
                }
            };
            let strategy_used = result.strategy.clone().unwrap_or_default();
            if result.ok {
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: format!("字段填写成功（策略：{strategy_used}）"),
                    stdout: format!("已填写字段 {selector}，使用策略：{strategy_used}"),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            } else {
                Some(tiangong_core::tool::ToolResult {
                    ok: false,
                    summary: "字段填写失败".to_string(),
                    stdout: String::new(),
                    stderr: result.error.unwrap_or_default(),
                    exit_code: 1,
                    execution: None,
                })
            }
        })
    }

    fn handle_web_click(
        fetcher: &Arc<dyn PageFetcher>,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<tiangong_core::tool::ToolResult>> + Send>,
    > {
        let selector = call
            .arguments
            .get("selector")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let fetcher = fetcher.clone();
        Box::pin(async move {
            let result = match fetcher.click_element(&selector).await {
                Some(r) => r,
                None => {
                    return Some(tiangong_core::tool::ToolResult {
                        ok: false,
                        summary: "浏览器未打开，无法点击元素".to_string(),
                        stdout: String::new(),
                        stderr: "请先使用 web_fetch 打开页面".to_string(),
                        exit_code: 1,
                        execution: None,
                    });
                }
            };
            if result.ok {
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: format!("已点击元素 {}", selector),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            } else {
                Some(tiangong_core::tool::ToolResult {
                    ok: false,
                    summary: "点击元素失败".to_string(),
                    stdout: String::new(),
                    stderr: result.error.unwrap_or_default(),
                    exit_code: 1,
                    execution: None,
                })
            }
        })
    }
}

impl tiangong_core::tool_override::ToolOverrideHandler for BrowserToolOverride {
    fn handle(
        &self,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<tiangong_core::tool::ToolResult>> + Send>,
    > {
        match call.name.as_str() {
            "web_fetch" => Self::handle_web_fetch(&self.fetcher, call),
            "web_form_extract" => Self::handle_web_form_extract(&self.fetcher),
            "web_form_fill" => Self::handle_web_form_fill(&self.fetcher, call),
            "web_click" => Self::handle_web_click(&self.fetcher, call),
            _ => Box::pin(async { None }),
        }
    }
}
