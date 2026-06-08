use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::types::BrowserCommand;
use tiangong_core::browser_trait::{ElementCandidate, PageFetcher};

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

    fn locate_element(
        &self,
        query: &str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<tiangong_core::browser_trait::LocateElementResult>,
                > + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        let query = query.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let resp: crate::types::LocateElementResult = send_and_wait!(
                tx,
                BrowserCommand::LocateElement { query, response_tx },
                response_rx,
                15
            );
            Some(resp.into())
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

    fn truncate_text(text: &str, max_chars: usize) -> String {
        let mut chars = text.chars();
        let mut value: String = chars.by_ref().take(max_chars).collect();
        if chars.next().is_some() {
            value.push('…');
        }
        value
    }

    fn candidate_identity(candidate: &ElementCandidate) -> String {
        let mut parts = Vec::new();
        if !candidate.tag.is_empty() {
            parts.push(candidate.tag.clone());
        }
        if !candidate.role.is_empty() {
            parts.push(format!("role={}", candidate.role));
        }
        if !candidate.label.is_empty() {
            parts.push(format!(
                "label=\"{}\"",
                Self::truncate_text(&candidate.label, 60)
            ));
        }
        if !candidate.text.is_empty() && candidate.text != candidate.label {
            parts.push(format!(
                "text=\"{}\"",
                Self::truncate_text(&candidate.text, 60)
            ));
        }
        if parts.is_empty() {
            "未知元素".to_string()
        } else {
            parts.join(" ")
        }
    }

    fn format_target(target: Option<&ElementCandidate>, selector: &str) -> String {
        match target {
            Some(target) => {
                let actual_selector = if target.selector.is_empty() {
                    selector
                } else {
                    &target.selector
                };
                format!(
                    "目标：{}\n实际选择器：{}",
                    Self::candidate_identity(target),
                    actual_selector
                )
            }
            None => format!("实际选择器：{selector}"),
        }
    }

    fn format_candidates(candidates: &[ElementCandidate]) -> String {
        if candidates.is_empty() {
            return String::new();
        }

        let mut lines = vec!["候选元素：".to_string()];
        for (index, candidate) in candidates.iter().take(8).enumerate() {
            let selector = if candidate.selector.is_empty() {
                "(无选择器)"
            } else {
                &candidate.selector
            };
            let reason = if candidate.reason.is_empty() {
                "匹配"
            } else {
                &candidate.reason
            };
            let position = match (candidate.x, candidate.y) {
                (Some(x), Some(y)) => format!(" | 坐标：{x},{y}"),
                _ => String::new(),
            };
            lines.push(format!(
                "{}. {} | selector: {} | score: {} | reason: {}{}",
                index + 1,
                Self::candidate_identity(candidate),
                selector,
                candidate.score,
                reason,
                position
            ));
        }
        if candidates.len() > 8 {
            lines.push(format!("... 还有 {} 个候选", candidates.len() - 8));
        }
        lines.join("\n")
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
                if result.title.is_empty() {
                    format!("浏览器获取成功：{}", result.final_url)
                } else {
                    format!("浏览器获取成功：{}", result.title)
                }
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
                let actual_selector = result
                    .selector
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&selector);
                let target_text = Self::format_target(result.target.as_ref(), actual_selector);
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: format!("字段填写成功（策略：{strategy_used}）"),
                    stdout: format!(
                        "已填写字段。\n输入定位：{}\n{}\n使用策略：{}",
                        selector, target_text, strategy_used
                    ),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            } else {
                let candidates = Self::format_candidates(&result.candidates);
                let error = result.error.unwrap_or_default();
                let stderr = if candidates.is_empty() {
                    error
                } else {
                    format!("{error}\n{candidates}")
                };
                Some(tiangong_core::tool::ToolResult {
                    ok: false,
                    summary: "字段填写失败".to_string(),
                    stdout: String::new(),
                    stderr,
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
                let actual_selector = result
                    .selector
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&selector);
                let target_text = Self::format_target(result.target.as_ref(), actual_selector);
                let position = match (result.x, result.y) {
                    (Some(x), Some(y)) => format!("\n点击坐标：{x},{y}"),
                    _ => String::new(),
                };
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: format!("已点击元素 {}", actual_selector),
                    stdout: format!(
                        "已点击元素。\n输入定位：{}\n{}{}",
                        selector, target_text, position
                    ),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            } else {
                let candidates = Self::format_candidates(&result.candidates);
                let error = result.error.unwrap_or_default();
                let stderr = if candidates.is_empty() {
                    error
                } else {
                    format!("{error}\n{candidates}")
                };
                Some(tiangong_core::tool::ToolResult {
                    ok: false,
                    summary: "点击元素失败".to_string(),
                    stdout: String::new(),
                    stderr,
                    exit_code: 1,
                    execution: None,
                })
            }
        })
    }

    fn handle_web_locate_element(
        fetcher: &Arc<dyn PageFetcher>,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<tiangong_core::tool::ToolResult>> + Send>,
    > {
        let query = call
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let fetcher = fetcher.clone();
        Box::pin(async move {
            let result = match fetcher.locate_element(&query).await {
                Some(r) => r,
                None => {
                    return Some(tiangong_core::tool::ToolResult {
                        ok: false,
                        summary: "浏览器未打开，无法定位元素".to_string(),
                        stdout: String::new(),
                        stderr: "请先使用 web_fetch 打开页面".to_string(),
                        exit_code: 1,
                        execution: None,
                    });
                }
            };
            if result.ok {
                let target = result
                    .target
                    .as_ref()
                    .map(|t| Self::format_target(Some(t), &t.selector))
                    .unwrap_or_default();
                let candidates = Self::format_candidates(&result.candidates);
                let mut stdout = format!("定位成功。\n{target}");
                if !candidates.is_empty() {
                    stdout.push('\n');
                    stdout.push_str(&candidates);
                }
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: "元素定位成功".to_string(),
                    stdout,
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            } else {
                let candidates = Self::format_candidates(&result.candidates);
                let error = result.error.unwrap_or_default();
                let mut stdout = String::from("未找到匹配元素。");
                if !candidates.is_empty() {
                    stdout.push('\n');
                    stdout.push_str(&candidates);
                }
                let stderr = if error.is_empty() {
                    String::new()
                } else {
                    error
                };
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: "未找到匹配元素".to_string(),
                    stdout,
                    stderr,
                    exit_code: 0,
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
            "web_locate_element" => Self::handle_web_locate_element(&self.fetcher, call),
            _ => Box::pin(async { None }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(selector: &str, text: &str) -> ElementCandidate {
        ElementCandidate {
            selector: selector.to_string(),
            text: text.to_string(),
            tag: "button".to_string(),
            role: "button".to_string(),
            label: text.to_string(),
            score: 93,
            reason: "smart match".to_string(),
            x: Some(120),
            y: Some(80),
        }
    }

    #[test]
    fn format_candidates_includes_selector_and_reason() {
        let candidates = vec![candidate("button:nth-of-type(1)", "登录")];

        let output = BrowserToolOverride::format_candidates(&candidates);

        assert!(output.contains("候选元素"));
        assert!(output.contains("selector: button:nth-of-type(1)"));
        assert!(output.contains("role=button"));
        assert!(output.contains("reason: smart match"));
        assert!(output.contains("坐标：120,80"));
    }

    #[test]
    fn format_target_prefers_candidate_selector() {
        let candidate = candidate("#login", "登录");

        let output = BrowserToolOverride::format_target(Some(&candidate), ".fallback");

        assert!(output.contains("目标：button role=button label=\"登录\""));
        assert!(output.contains("实际选择器：#login"));
        assert!(!output.contains(".fallback"));
    }

    #[test]
    fn format_target_without_candidate_uses_fallback_selector() {
        let output = BrowserToolOverride::format_target(None, ".btn-primary");

        assert!(!output.contains("目标"));
        assert!(output.contains("实际选择器：.btn-primary"));
    }

    #[test]
    fn format_candidates_truncates_at_eight() {
        let candidates: Vec<ElementCandidate> = (0..12)
            .map(|i| ElementCandidate {
                selector: format!("#item-{i}"),
                text: format!("Item {i}"),
                tag: "div".to_string(),
                role: String::new(),
                label: String::new(),
                score: 50,
                reason: "match".to_string(),
                x: None,
                y: None,
            })
            .collect();

        let output = BrowserToolOverride::format_candidates(&candidates);

        assert!(output.contains("#item-0"));
        assert!(output.contains("#item-7"));
        assert!(!output.contains("#item-8"));
        assert!(output.contains("还有 4 个候选"));
    }

    #[test]
    fn format_candidates_empty_returns_empty_string() {
        let output = BrowserToolOverride::format_candidates(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn candidate_identity_with_all_fields() {
        let c = ElementCandidate {
            selector: "#email".to_string(),
            text: "邮箱地址".to_string(),
            tag: "input".to_string(),
            role: "textbox".to_string(),
            label: "邮箱".to_string(),
            score: 90,
            reason: String::new(),
            x: None,
            y: None,
        };
        let id = BrowserToolOverride::candidate_identity(&c);
        assert!(id.contains("input"));
        assert!(id.contains("role=textbox"));
        assert!(id.contains("label=\"邮箱\""));
        assert!(id.contains("text=\"邮箱地址\""));
    }

    #[test]
    fn candidate_identity_label_equals_text_skips_duplicate() {
        let c = ElementCandidate {
            selector: "#btn".to_string(),
            text: "提交".to_string(),
            tag: "button".to_string(),
            role: "button".to_string(),
            label: "提交".to_string(),
            score: 80,
            reason: String::new(),
            x: None,
            y: None,
        };
        let id = BrowserToolOverride::candidate_identity(&c);
        // text 和 label 相同时只显示 label
        assert!(id.contains("label=\"提交\""));
        assert!(!id.contains("text=\"提交\""));
    }

    #[test]
    fn truncate_text_under_limit_keeps_full() {
        let result = BrowserToolOverride::truncate_text("hello", 10);
        assert_eq!(result, "hello");
    }

    #[test]
    fn truncate_text_over_limit_adds_ellipsis() {
        let result = BrowserToolOverride::truncate_text("abcdefghij", 5);
        assert_eq!(result, "abcde…");
    }
}
