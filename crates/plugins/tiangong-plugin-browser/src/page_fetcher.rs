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
                events: snapshot.events.into_iter().map(Into::into).collect(),
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
        wait_for: Option<&str>,
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
        let wait_for = wait_for.map(|s| s.to_string());
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let result: crate::types::FillFieldResult = send_and_wait!(
                tx,
                BrowserCommand::FormFill {
                    selector,
                    value,
                    strategy,
                    wait_for,
                    response_tx,
                },
                response_rx,
                15
            );
            Some(result.into())
        })
    }

    fn click_element(
        &self,
        selector: &str,
        wait_for: Option<&str>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Option<tiangong_core::browser_trait::ClickElementResult>,
                > + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        let selector = selector.to_string();
        let wait_for = wait_for.map(|s| s.to_string());
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let result: crate::types::ClickElementResult = send_and_wait!(
                tx,
                BrowserCommand::ClickElement {
                    selector,
                    wait_for,
                    response_tx
                },
                response_rx,
                15
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

    fn query_dom(
        &self,
        selector: &str,
        max_results: usize,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<tiangong_core::browser_trait::QueryDomResult>>
                + Send,
        >,
    > {
        let tx = self.cmd_tx.clone();
        let selector = selector.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let result: crate::types::QueryDomResult = send_and_wait!(
                tx,
                BrowserCommand::QueryDom {
                    selector,
                    max_results,
                    response_tx
                },
                response_rx,
                10
            );
            Some(result.into())
        })
    }
}

/// 浏览器工具覆盖处理器（web_fetch / web_form_extract / web_form_fill / web_click / web_load_html / web_query_dom）
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
            if !result.ok {
                return Some(tiangong_core::tool::ToolResult {
                    ok: false,
                    summary: format!(
                        "浏览器获取失败：{}",
                        result.error.clone().unwrap_or_default()
                    ),
                    stdout: String::new(),
                    stderr: result.error.unwrap_or_default(),
                    exit_code: 1,
                    execution: None,
                });
            }
            // 桌面模式：浏览器已导航到目标 URL，页面内容由 observe_page 自动推送。
            // 只返回摘要信息，避免与浏览器推送数据重复。
            Some(tiangong_core::tool::ToolResult {
                ok: true,
                summary: format!("浏览器已打开：{}", result.title),
                stdout: format!(
                    "已在浏览器中打开页面\n标题：{}\nURL：{}\n\n页面内容将通过浏览器自动推送。",
                    result.title, result.final_url
                ),
                stderr: String::new(),
                exit_code: 0,
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
            let total_buttons: usize = result.forms.iter().map(|f| f.buttons.len()).sum();
            let json_output = serde_json::to_string_pretty(&result)
                .unwrap_or_else(|_| "序列化表单数据失败".to_string());

            // 人类可读摘要
            let mut lines = Vec::new();
            for (fi, form) in result.forms.iter().enumerate() {
                if result.forms.len() > 1 {
                    lines.push(format!("表单 {}:", fi + 1));
                }
                for field in &form.fields {
                    let mut desc = format!(
                        "  字段 {}: {} type={} label=\"{}\"",
                        field.index + 1,
                        field.selector,
                        field.field_type,
                        field.label
                    );
                    if !field.placeholder.is_empty() {
                        desc.push_str(&format!(" placeholder=\"{}\"", field.placeholder));
                    }
                    if field.required {
                        desc.push_str(" [必填]");
                    }
                    if field.readonly {
                        desc.push_str(" [只读]");
                    }
                    if field.disabled {
                        desc.push_str(" [禁用]");
                    }
                    if !field.options.is_empty() {
                        let opts: Vec<String> = field
                            .options
                            .iter()
                            .map(|o| format!("{}={}", o.text, o.value))
                            .collect();
                        desc.push_str(&format!(" 选项：[{}]", opts.join(", ")));
                    }
                    lines.push(desc);
                }
                for (bi, btn) in form.buttons.iter().enumerate() {
                    let state = if btn.disabled { "disabled" } else { "enabled" };
                    lines.push(format!(
                        "  按钮 {}: <{}> [{}] ({}) {}",
                        bi + 1,
                        btn.tag,
                        state,
                        btn.selector,
                        btn.text
                    ));
                }
            }
            let human_summary = lines.join("\n");

            Some(tiangong_core::tool::ToolResult {
                ok: true,
                summary: format!(
                    "提取到 {} 个表单，共 {} 个字段，{} 个按钮",
                    result.forms.len(),
                    total_fields,
                    total_buttons
                ),
                stdout: format!("{human_summary}\n\n--- JSON ---\n{json_output}"),
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
        let wait_for = call
            .arguments
            .get("wait_for")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let fetcher = fetcher.clone();
        Box::pin(async move {
            let result = match fetcher
                .form_fill(&selector, &value, &strategy, wait_for.as_deref())
                .await
            {
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
                let wait_info = match &result.wait_result {
                    Some(w) if w.ok => {
                        format!("，等待条件满足：{}（{}ms）", w.condition, w.elapsed_ms)
                    }
                    Some(w) => format!("，等待超时：{}", w.error.as_deref().unwrap_or("超时")),
                    None => String::new(),
                };
                let diff_info = match &result.page_diff {
                    Some(d) if !d.is_empty() => format!("\n{d}"),
                    _ => String::new(),
                };
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: format!("字段填写成功（策略：{strategy_used}）{wait_info}{diff_info}"),
                    stdout: format!(
                        "已填写字段 {selector}，使用策略：{strategy_used}{wait_info}{diff_info}"
                    ),
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
        let wait_for = call
            .arguments
            .get("wait_for")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let fetcher = fetcher.clone();
        Box::pin(async move {
            let result = match fetcher.click_element(&selector, wait_for.as_deref()).await {
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
                let wait_info = match &result.wait_result {
                    Some(w) if w.ok => {
                        format!("，等待条件满足：{}（{}ms）", w.condition, w.elapsed_ms)
                    }
                    Some(w) => format!("，等待超时：{}", w.error.as_deref().unwrap_or("超时")),
                    None => String::new(),
                };
                let diff_info = match &result.page_diff {
                    Some(d) if !d.is_empty() => format!("\n{d}"),
                    _ => String::new(),
                };
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: format!("已点击元素 {selector}{wait_info}{diff_info}"),
                    stdout: diff_info,
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            } else {
                let candidates_info = if result.candidates.is_empty() {
                    String::new()
                } else {
                    let cands: Vec<String> = result
                        .candidates
                        .iter()
                        .map(|c| format!("<{}> {} ({})", c.tag, c.text, c.selector))
                        .collect();
                    format!("。可能的目标：{}", cands.join("、"))
                };
                Some(tiangong_core::tool::ToolResult {
                    ok: false,
                    summary: "点击元素失败".to_string(),
                    stdout: String::new(),
                    stderr: format!("{}{candidates_info}", result.error.unwrap_or_default()),
                    exit_code: 1,
                    execution: None,
                })
            }
        })
    }

    fn handle_web_query_dom(
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
        let max_results = call
            .arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;
        let fetcher = fetcher.clone();
        Box::pin(async move {
            let result = match fetcher.query_dom(&selector, max_results).await {
                Some(r) => r,
                None => {
                    return Some(tiangong_core::tool::ToolResult {
                        ok: false,
                        summary: "浏览器未打开，无法查询 DOM".to_string(),
                        stdout: String::new(),
                        stderr: "请先使用 web_fetch 打开页面".to_string(),
                        exit_code: 1,
                        execution: None,
                    });
                }
            };
            if result.elements.is_empty() {
                return Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: format!("选择器 \"{}\" 无匹配元素（共 0 个）", result.selector),
                    stdout: format!("选择器 \"{}\" 未匹配到任何元素", result.selector),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                });
            }
            let mut lines = Vec::new();
            for el in &result.elements {
                let mut attrs_parts = Vec::new();
                for (k, v) in &el.attributes {
                    attrs_parts.push(format!("{k}=\"{v}\""));
                }
                let attrs_str = if attrs_parts.is_empty() {
                    String::new()
                } else {
                    format!(" {}", attrs_parts.join(" "))
                };
                lines.push(format!("元素 {}: <{}>{}", el.index, el.tag, attrs_str));
                if !el.text.is_empty() {
                    let text_preview = if el.text.len() > 200 {
                        format!("{}...", &el.text[..200])
                    } else {
                        el.text.clone()
                    };
                    for line in text_preview.split('\n') {
                        if !line.trim().is_empty() {
                            lines.push(format!("  {}", line.trim()));
                        }
                    }
                }
                lines.push(format!("  选择器: {}", el.selector));
            }
            let human_output = lines.join("\n");
            let json_output = serde_json::to_string_pretty(&result)
                .unwrap_or_else(|_| "序列化结果失败".to_string());
            Some(tiangong_core::tool::ToolResult {
                ok: true,
                summary: format!(
                    "选择器 \"{}\"：共 {} 个匹配（返回 {} 个）",
                    result.selector, result.total, result.returned
                ),
                stdout: format!("{human_output}\n\n--- JSON ---\n{json_output}"),
                stderr: String::new(),
                exit_code: 0,
                execution: None,
            })
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
                Some(tiangong_core::tool::ToolResult {
                    ok: true,
                    summary: "未找到匹配元素".to_string(),
                    stdout,
                    stderr: error,
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
        _session: &tiangong_core::session::Session,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<tiangong_core::tool::ToolResult>> + Send>,
    > {
        match call.name.as_str() {
            "web_fetch" => Self::handle_web_fetch(&self.fetcher, call),
            "web_form_extract" => Self::handle_web_form_extract(&self.fetcher),
            "web_form_fill" => Self::handle_web_form_fill(&self.fetcher, call),
            "web_click" => Self::handle_web_click(&self.fetcher, call),
            "web_query_dom" => Self::handle_web_query_dom(&self.fetcher, call),
            "web_locate_element" => Self::handle_web_locate_element(&self.fetcher, call),
            _ => Box::pin(async { None }),
        }
    }
}

/// 浏览器内容注入（ToolInput 实现）。
///
/// 页面加载完成或内容变化时，通过 AgentInput trait 统一投递到 Agent 对话链。
/// tool_name 为 `browser_data`，render 返回结构化 JSON。
pub struct BrowserContent {
    pub title: String,
    pub url: String,
    pub text: String,
    pub tabs: Vec<(String, String, String)>,
    pub active_tab_id: Option<String>,
    pub feedback: Option<String>,
}

impl tiangong_core::agent_input::ToolInput for BrowserContent {
    fn tool_name(&self) -> &str {
        "browser_data"
    }

    fn render(&self) -> serde_json::Value {
        serde_json::json!({
            "title": self.title,
            "url": self.url,
            "text": self.text,
            "tabs": self.tabs,
            "active_tab_id": self.active_tab_id,
            "feedback": self.feedback,
        })
    }
}
