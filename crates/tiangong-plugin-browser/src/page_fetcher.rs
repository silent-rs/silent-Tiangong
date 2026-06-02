use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::types::BrowserCommand;
use tiangong_core::browser_trait::{FetchResult, PageFetcher, PageSnapshot};

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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FetchResult>> + Send>> {
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
                Ok(Ok(resp)) => Some(FetchResult {
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<PageSnapshot>> + Send>> {
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

            Some(PageSnapshot {
                title: snapshot.title,
                url: snapshot.url,
                text: snapshot.text,
            })
        })
    }
}

/// 浏览器工具覆盖处理器（web_fetch 和 web_browse）
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
                _ => None,
            }
        })
    }
}
