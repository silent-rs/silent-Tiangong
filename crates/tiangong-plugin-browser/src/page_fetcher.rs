use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::types::BrowserCommand;
use tiangong_core::browser_trait::{
    ClickElementResult as CoreClickResult, FillFieldResult as CoreFillResult,
    FormExtractResult as CoreFormExtractResult, PageFetcher, TabInfo as CoreTabInfo,
    TabListResult as CoreTabListResult,
};

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
            let cmd = BrowserCommand::FetchPage {
                url,
                max_chars,
                response_tx,
            };

            if tx.send(cmd).await.is_err() {
                return None;
            }

            match tokio::time::timeout(Duration::from_secs(30), response_rx).await {
                Ok(Ok(resp)) => Some(tiangong_core::browser_trait::FetchResult {
                    ok: resp.ok,
                    title: resp.title,
                    content: resp.content,
                    final_url: resp.final_url,
                    error: resp.error,
                }),
                _ => None,
            }
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
            let cmd = BrowserCommand::ObservePage { response_tx };

            if tx.send(cmd).await.is_err() {
                return None;
            }

            let snapshot = tokio::time::timeout(Duration::from_secs(10), response_rx)
                .await
                .ok()?
                .ok()?;

            Some(tiangong_core::browser_trait::PageSnapshot {
                title: snapshot.title,
                url: snapshot.url,
                text: snapshot.text,
                tabs: snapshot
                    .tabs
                    .into_iter()
                    .map(|t| CoreTabInfo {
                        id: t.id,
                        url: t.url,
                        title: t.title,
                    })
                    .collect(),
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
            let cmd = BrowserCommand::TabList { response_tx };

            if tx.send(cmd).await.is_err() {
                return None;
            }

            let tabs = tokio::time::timeout(Duration::from_secs(10), response_rx)
                .await
                .ok()?
                .ok()?;

            Some(CoreTabListResult {
                tabs: tabs
                    .into_iter()
                    .map(|t| CoreTabInfo {
                        id: t.id,
                        url: t.url,
                        title: t.title,
                    })
                    .collect(),
                active_tab_id: None,
            })
        })
    }

    fn form_extract(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<CoreFormExtractResult>> + Send>>
    {
        let tx = self.cmd_tx.clone();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let cmd = BrowserCommand::FormExtract { response_tx };

            if tx.send(cmd).await.is_err() {
                return None;
            }

            let result = tokio::time::timeout(Duration::from_secs(10), response_rx)
                .await
                .ok()?
                .ok()?;

            Some(CoreFormExtractResult {
                forms: result
                    .forms
                    .into_iter()
                    .map(|f| tiangong_core::browser_trait::FormInfo {
                        fields: f
                            .fields
                            .into_iter()
                            .map(|field| tiangong_core::browser_trait::FormField {
                                index: field.index,
                                tag: field.tag,
                                field_type: field.field_type,
                                name: field.name,
                                id: field.id,
                                label: field.label,
                                placeholder: field.placeholder,
                                value: field.value,
                                required: field.required,
                                readonly: field.readonly,
                                disabled: field.disabled,
                                selector: field.selector,
                                options: field
                                    .options
                                    .into_iter()
                                    .map(|o| tiangong_core::browser_trait::SelectOption {
                                        value: o.value,
                                        text: o.text,
                                    })
                                    .collect(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
        })
    }

    fn form_fill(
        &self,
        selector: &str,
        value: &str,
        strategy: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<CoreFillResult>> + Send>> {
        let tx = self.cmd_tx.clone();
        let selector = selector.to_string();
        let value = value.to_string();
        let strategy = strategy.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let cmd = BrowserCommand::FormFill {
                selector,
                value,
                strategy,
                response_tx,
            };

            if tx.send(cmd).await.is_err() {
                return None;
            }

            let result = tokio::time::timeout(Duration::from_secs(10), response_rx)
                .await
                .ok()?
                .ok()?;

            Some(CoreFillResult {
                ok: result.ok,
                strategy: result.strategy,
                error: result.error,
                current_value: result.current_value,
            })
        })
    }

    fn click_element(
        &self,
        selector: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<CoreClickResult>> + Send>> {
        let tx = self.cmd_tx.clone();
        let selector = selector.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let cmd = BrowserCommand::ClickElement {
                selector,
                response_tx,
            };

            if tx.send(cmd).await.is_err() {
                return None;
            }

            let result = tokio::time::timeout(Duration::from_secs(10), response_rx)
                .await
                .ok()?
                .ok()?;

            Some(CoreClickResult {
                ok: result.ok,
                error: result.error,
            })
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
            let cmd = BrowserCommand::LoadHtml { html, response_tx };

            if tx.send(cmd).await.is_err() {
                return None;
            }

            tokio::time::timeout(Duration::from_secs(10), response_rx)
                .await
                .ok()?
                .ok()
        })
    }
}

/// 浏览器工具覆盖处理器（web_fetch / web_browse / web_form_extract / web_form_fill / web_click / web_load_html）
pub struct BrowserToolOverride {
    fetcher: Arc<dyn PageFetcher>,
}

impl BrowserToolOverride {
    pub fn new(fetcher: Arc<dyn PageFetcher>) -> Self {
        Self { fetcher }
    }
}

impl tiangong_core::tool_override::ToolOverrideHandler for BrowserToolOverride {
    fn handle(
        &self,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<tiangong_core::tool::ToolResult>> + Send>,
    > {
        let fetcher = self.fetcher.clone();
        let call = call.clone();
        Box::pin(async move {
            match call.name.as_str() {
                "web_fetch" => {
                    let mode = call
                        .arguments
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("text");
                    if mode != "text" {
                        return None;
                    }
                    let url = call.arguments.get("url")?.as_str()?.to_string();
                    let max_chars = call
                        .arguments
                        .get("max_chars")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(12000) as usize;
                    let result = fetcher.fetch_page(&url, max_chars).await?;
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
                }
                "web_browse" => {
                    let snapshot = fetcher.observe_page().await?;
                    let content = if snapshot.text.is_empty() {
                        format!(
                            "浏览器页面：{}\nURL：{}\n状态：页面内容为空",
                            snapshot.title, snapshot.url
                        )
                    } else {
                        format!(
                            "浏览器页面：{}\nURL：{}\n\n{}",
                            snapshot.title, snapshot.url, snapshot.text
                        )
                    };
                    Some(tiangong_core::tool::ToolResult {
                        ok: true,
                        summary: format!("浏览器当前页面：{}", snapshot.title),
                        stdout: content,
                        stderr: String::new(),
                        exit_code: 0,
                        execution: None,
                    })
                }
                "web_form_extract" => {
                    let result = fetcher.form_extract().await?;
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
                }
                "web_form_fill" => {
                    let selector = call
                        .arguments
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let value = call
                        .arguments
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let strategy = call
                        .arguments
                        .get("strategy")
                        .and_then(|v| v.as_str())
                        .unwrap_or("auto");
                    let result = fetcher.form_fill(selector, value, strategy).await?;
                    let strategy_used = result.strategy.clone().unwrap_or_default();
                    if result.ok {
                        Some(tiangong_core::tool::ToolResult {
                            ok: true,
                            summary: format!("字段填写成功（策略：{strategy_used}）",),
                            stdout: format!("已填写字段 {selector}，使用策略：{strategy_used}",),
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
                }
                "web_click" => {
                    let selector = call
                        .arguments
                        .get("selector")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let result = fetcher.click_element(selector).await?;
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
                }
                "web_load_html" => {
                    let html = call
                        .arguments
                        .get("html")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let result = fetcher.load_html(html).await?;
                    match result {
                        Ok(()) => Some(tiangong_core::tool::ToolResult {
                            ok: true,
                            summary: "HTML 内容已加载到浏览器".to_string(),
                            stdout: String::new(),
                            stderr: String::new(),
                            exit_code: 0,
                            execution: None,
                        }),
                        Err(err) => Some(tiangong_core::tool::ToolResult {
                            ok: false,
                            summary: "加载 HTML 失败".to_string(),
                            stdout: String::new(),
                            stderr: err,
                            exit_code: 1,
                            execution: None,
                        }),
                    }
                }
                _ => None,
            }
        })
    }
}
