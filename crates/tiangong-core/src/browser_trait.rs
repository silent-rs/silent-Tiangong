use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

/// 浏览器页面获取能力抽象。
///
/// GUI 模式下由 tiangong-plugin-browser 实现，CLI/Server 模式下为 None（回退到 HTTP web_fetch）。
pub trait PageFetcher: Send + Sync + 'static {
    /// 获取指定 URL 的页面内容。
    /// 返回 None 表示能力不可用，调用方应回退到 HTTP 获取。
    fn fetch_page(
        &self,
        url: &str,
        max_chars: usize,
    ) -> Pin<Box<dyn Future<Output = Option<FetchResult>> + Send>>;

    /// 获取当前浏览器页面的快照。
    /// 返回 None 表示浏览器未打开或能力不可用。
    fn observe_page(&self) -> Pin<Box<dyn Future<Output = Option<PageSnapshot>> + Send>>;

    /// 获取标签列表。
    fn list_tabs(&self) -> Pin<Box<dyn Future<Output = Option<TabListResult>> + Send>> {
        Box::pin(async move { None })
    }

    /// 提取当前页面的表单结构。
    fn form_extract(&self) -> Pin<Box<dyn Future<Output = Option<FormExtractResult>> + Send>> {
        Box::pin(async move { None })
    }

    /// 填写表单字段。
    fn form_fill(
        &self,
        _selector: &str,
        _value: &str,
        _strategy: &str,
        _wait_for: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = Option<FillFieldResult>> + Send>> {
        Box::pin(async move { None })
    }

    /// 点击页面元素。
    fn click_element(
        &self,
        _selector: &str,
        _wait_for: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = Option<ClickElementResult>> + Send>> {
        Box::pin(async move { None })
    }

    /// 加载 HTML 内容到浏览器。
    #[allow(clippy::type_complexity)]
    fn load_html(
        &self,
        _html: &str,
    ) -> Pin<Box<dyn Future<Output = Option<Result<(), String>>> + Send>> {
        Box::pin(async move { None })
    }

    /// 智能元素定位（不执行操作，仅查询候选）。
    fn locate_element(
        &self,
        _query: &str,
    ) -> Pin<Box<dyn Future<Output = Option<LocateElementResult>> + Send>> {
        Box::pin(async move { None })
    }

    /// 用 CSS 选择器查询当前页面的 DOM 元素。
    fn query_dom(
        &self,
        _selector: &str,
        _max_results: usize,
    ) -> Pin<Box<dyn Future<Output = Option<QueryDomResult>> + Send>> {
        Box::pin(async move { None })
    }
}

/// 页面获取结果（纯数据，无 tokio 依赖）
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub ok: bool,
    pub title: String,
    pub content: String,
    pub final_url: String,
    pub error: Option<String>,
}

/// 页面快照（纯数据，无 tokio 依赖）
#[derive(Debug, Clone)]
pub struct PageSnapshot {
    pub title: String,
    pub url: String,
    pub text: String,
    pub tabs: Vec<TabInfo>,
    pub active_tab_id: Option<String>,
    pub events: Vec<BrowserEvent>,
}

/// 浏览器观测事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserEvent {
    #[serde(rename = "dialog_opened")]
    DialogOpened {
        timestamp: u64,
        #[serde(default)]
        detail: String,
    },
    #[serde(rename = "dialog_closed")]
    DialogClosed { timestamp: u64 },
    #[serde(rename = "content_changed")]
    ContentChanged {
        timestamp: u64,
        #[serde(default)]
        detail: String,
    },
    #[serde(rename = "user_click")]
    UserClick {
        timestamp: u64,
        element: String,
        text: String,
        selector: String,
    },
    #[serde(rename = "user_input")]
    UserInput {
        timestamp: u64,
        selector: String,
        label: String,
        value_length: usize,
    },
    #[serde(rename = "user_navigation")]
    UserNavigation { timestamp: u64, url: String },
    #[serde(rename = "network_response")]
    NetworkResponse {
        timestamp: u64,
        url: String,
        method: String,
        status: u16,
        #[serde(default)]
        detail: String,
    },
}

pub fn format_browser_events(events: &[BrowserEvent]) -> Option<String> {
    if events.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    for event in events {
        match event {
            BrowserEvent::DialogOpened { detail, .. } => {
                lines.push("[页面变化] 出现新的弹窗/覆盖层".to_string());
                push_preview(&mut lines, detail, 1200);
            }
            BrowserEvent::DialogClosed { .. } => {
                lines.push("[页面变化] 弹窗/覆盖层已关闭".to_string());
            }
            BrowserEvent::ContentChanged { detail, .. } => {
                lines.push("[页面变化] 页面内容已更新".to_string());
                push_preview(&mut lines, detail, 1000);
            }
            BrowserEvent::UserClick {
                element,
                text,
                selector,
                ..
            } => {
                let mut desc = format!("[用户操作] 点击 <{element}>");
                if !text.trim().is_empty() {
                    desc.push_str(&format!(" {}", text.trim()));
                }
                if !selector.trim().is_empty() {
                    desc.push_str(&format!(" ({selector})"));
                }
                lines.push(desc);
            }
            BrowserEvent::UserInput {
                selector,
                label,
                value_length,
                ..
            } => {
                let target = if label.trim().is_empty() {
                    selector.as_str()
                } else {
                    label.as_str()
                };
                lines.push(format!(
                    "[用户操作] 输入字段 {target} 已变化（长度 {value_length}）"
                ));
            }
            BrowserEvent::UserNavigation { url, .. } => {
                lines.push(format!("[用户操作] 页面导航到 {url}"));
            }
            BrowserEvent::NetworkResponse {
                url,
                method,
                status,
                detail,
                ..
            } => {
                lines.push(format!(
                    "[网络响应] {} {} (状态 {})",
                    method,
                    truncate_chars(url, 120),
                    status
                ));
                push_preview(&mut lines, detail, 500);
            }
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn push_preview(lines: &mut Vec<String>, text: &str, max_chars: usize) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    lines.push(truncate_chars(text, max_chars));
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

/// 标签信息
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
}

/// 表单字段信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormField {
    pub index: usize,
    pub tag: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub name: String,
    pub id: String,
    pub label: String,
    pub placeholder: String,
    pub value: String,
    pub required: bool,
    pub readonly: bool,
    pub disabled: bool,
    pub selector: String,
    #[serde(default)]
    pub options: Vec<SelectOption>,
}

/// select 选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub text: String,
}

/// 表单按钮
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormButton {
    pub tag: String,
    #[serde(rename = "type")]
    pub button_type: String,
    pub text: String,
    pub disabled: bool,
    pub selector: String,
}

/// 表单信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormInfo {
    pub fields: Vec<FormField>,
    #[serde(default)]
    pub buttons: Vec<FormButton>,
}

/// 表单提取结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormExtractResult {
    pub forms: Vec<FormInfo>,
}

/// 智能定位候选元素
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ElementCandidate {
    pub selector: String,
    pub text: String,
    pub tag: String,
    pub role: String,
    pub label: String,
    pub score: i32,
    pub reason: String,
    pub x: Option<i32>,
    pub y: Option<i32>,
}

/// 字段填写结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillFieldResult {
    pub ok: bool,
    pub strategy: Option<String>,
    pub error: Option<String>,
    #[serde(rename = "currentValue", default)]
    pub current_value: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub target: Option<ElementCandidate>,
    #[serde(default)]
    pub candidates: Vec<ElementCandidate>,
    #[serde(default)]
    pub wait_result: Option<WaitResult>,
    #[serde(default)]
    pub page_diff: Option<String>,
}

/// 元素点击结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickElementResult {
    pub ok: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub target: Option<ElementCandidate>,
    #[serde(default)]
    pub candidates: Vec<ElementCandidate>,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    #[serde(default)]
    pub wait_result: Option<WaitResult>,
    #[serde(default)]
    pub page_diff: Option<String>,
}

/// 等待条件结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitResult {
    pub ok: bool,
    pub condition: String,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

/// 标签列表结果
#[derive(Debug, Clone)]
pub struct TabListResult {
    pub tabs: Vec<TabInfo>,
    pub active_tab_id: Option<String>,
}

/// 智能元素定位结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateElementResult {
    pub ok: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub ambiguous: bool,
    #[serde(default)]
    pub target: Option<ElementCandidate>,
    #[serde(default)]
    pub candidates: Vec<ElementCandidate>,
}

/// DOM 查询结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryDomResult {
    pub selector: String,
    pub total: usize,
    pub returned: usize,
    pub elements: Vec<QueryDomElement>,
}

/// DOM 查询到的单个元素
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryDomElement {
    pub index: usize,
    pub tag: String,
    pub text: String,
    pub attributes: std::collections::HashMap<String, String>,
    pub selector: String,
    pub rect: DomRect,
}

/// DOM 元素的矩形位置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_browser_events_includes_small_page_and_network_feedback() {
        let events = vec![
            BrowserEvent::DialogOpened {
                timestamp: 1,
                detail: "创建 API key sk-test".to_string(),
            },
            BrowserEvent::NetworkResponse {
                timestamp: 2,
                url: "https://platform.deepseek.com/api_keys".to_string(),
                method: "POST".to_string(),
                status: 200,
                detail: "{\"key\":\"sk-test\"}".to_string(),
            },
        ];

        let text = format_browser_events(&events).unwrap();
        assert!(text.contains("[页面变化]"));
        assert!(text.contains("[网络响应] POST https://platform.deepseek.com/api_keys (状态 200)"));
        assert!(text.contains("sk-test"));
    }

    #[test]
    fn format_browser_events_returns_none_for_empty_events() {
        assert!(format_browser_events(&[]).is_none());
    }
}
