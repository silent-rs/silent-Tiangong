use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// 浏览器标签
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserTab {
    pub id: String,
    pub url: String,
    pub title: String,
}

/// 浏览器命令（内部通道消息）
pub enum BrowserCommand {
    /// 获取网页内容（替代 web_fetch）
    FetchPage {
        url: String,
        max_chars: usize,
        response_tx: oneshot::Sender<BrowserResponse>,
    },
    /// 打开 URL（用于链接点击等场景）
    OpenUrl { url: String },
    /// 获取当前浏览器页面的快照
    ObservePage {
        response_tx: oneshot::Sender<BrowserPageSnapshot>,
    },
    /// 提取页面表单结构
    FormExtract {
        response_tx: oneshot::Sender<FormExtractResult>,
    },
    /// 填写表单字段
    FormFill {
        selector: String,
        value: String,
        strategy: String,
        wait_for: Option<String>,
        response_tx: oneshot::Sender<FillFieldResult>,
    },
    /// 点击页面元素
    ClickElement {
        selector: String,
        wait_for: Option<String>,
        response_tx: oneshot::Sender<ClickElementResult>,
    },
    /// 加载本地 HTML 内容
    LoadHtml {
        html: String,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
    /// 获取标签列表
    TabList {
        response_tx: oneshot::Sender<Vec<BrowserTab>>,
    },
    /// 新建标签
    TabNew { url: String },
    /// 切换标签
    TabSwitch { tab_id: String },
    /// 关闭标签
    TabClose { tab_id: String },
    /// 提取批注区域的元素信息
    AnnotationExtract {
        response_tx: oneshot::Sender<AnnotationExtractResult>,
    },
    /// 用 CSS 选择器查询 DOM 元素
    QueryDom {
        selector: String,
        max_results: usize,
        response_tx: oneshot::Sender<QueryDomResult>,
    },
}

/// 浏览器响应
#[derive(Debug, Clone)]
pub struct BrowserResponse {
    pub ok: bool,
    pub title: String,
    pub content: String,
    pub final_url: String,
    pub error: Option<String>,
}

/// 浏览器页面快照
#[derive(Debug, Clone)]
pub struct BrowserPageSnapshot {
    pub title: String,
    pub url: String,
    pub text: String,
    pub status: PageStatus,
    pub tabs: Vec<BrowserTab>,
    pub active_tab_id: Option<String>,
}

/// 页面状态
#[derive(Debug, Clone)]
pub enum PageStatus {
    Loading,
    Loaded,
    Error(String),
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

/// 字段填写结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillFieldResult {
    pub ok: bool,
    pub strategy: Option<String>,
    pub error: Option<String>,
    #[serde(rename = "currentValue")]
    pub current_value: Option<String>,
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
    pub wait_result: Option<WaitResult>,
    #[serde(default)]
    pub candidates: Vec<ElementCandidate>,
    #[serde(default)]
    pub page_diff: Option<String>,
}

/// 等待条件结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitResult {
    pub ok: bool,
    pub condition: String,
    #[serde(rename = "elapsed")]
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

/// 候选元素信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementCandidate {
    pub tag: String,
    pub text: String,
    pub selector: String,
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
    pub attributes: HashMap<String, String>,
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

/// 浏览器语义事件（由 observer 模块产生）
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
}

// ── 类型转换：plugin → core ──────────────────────────────────────────

impl From<BrowserTab> for tiangong_core::browser_trait::TabInfo {
    fn from(t: BrowserTab) -> Self {
        Self {
            id: t.id,
            url: t.url,
            title: t.title,
        }
    }
}

impl From<BrowserResponse> for tiangong_core::browser_trait::FetchResult {
    fn from(r: BrowserResponse) -> Self {
        Self {
            ok: r.ok,
            title: r.title,
            content: r.content,
            final_url: r.final_url,
            error: r.error,
        }
    }
}

impl From<SelectOption> for tiangong_core::browser_trait::SelectOption {
    fn from(o: SelectOption) -> Self {
        Self {
            value: o.value,
            text: o.text,
        }
    }
}

impl From<FormField> for tiangong_core::browser_trait::FormField {
    fn from(f: FormField) -> Self {
        Self {
            index: f.index,
            tag: f.tag,
            field_type: f.field_type,
            name: f.name,
            id: f.id,
            label: f.label,
            placeholder: f.placeholder,
            value: f.value,
            required: f.required,
            readonly: f.readonly,
            disabled: f.disabled,
            selector: f.selector,
            options: f.options.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FormButton> for tiangong_core::browser_trait::FormButton {
    fn from(b: FormButton) -> Self {
        Self {
            tag: b.tag,
            button_type: b.button_type,
            text: b.text,
            disabled: b.disabled,
            selector: b.selector,
        }
    }
}

impl From<FormInfo> for tiangong_core::browser_trait::FormInfo {
    fn from(f: FormInfo) -> Self {
        Self {
            fields: f.fields.into_iter().map(Into::into).collect(),
            buttons: f.buttons.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FormExtractResult> for tiangong_core::browser_trait::FormExtractResult {
    fn from(r: FormExtractResult) -> Self {
        Self {
            forms: r.forms.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FillFieldResult> for tiangong_core::browser_trait::FillFieldResult {
    fn from(r: FillFieldResult) -> Self {
        Self {
            ok: r.ok,
            strategy: r.strategy,
            error: r.error,
            current_value: r.current_value,
            wait_result: r.wait_result.map(Into::into),
            page_diff: r.page_diff,
        }
    }
}

impl From<ClickElementResult> for tiangong_core::browser_trait::ClickElementResult {
    fn from(r: ClickElementResult) -> Self {
        Self {
            ok: r.ok,
            error: r.error,
            wait_result: r.wait_result.map(Into::into),
            candidates: r
                .candidates
                .into_iter()
                .map(|c| tiangong_core::browser_trait::ElementCandidate {
                    tag: c.tag,
                    text: c.text,
                    selector: c.selector,
                })
                .collect(),
            page_diff: r.page_diff,
        }
    }
}

impl From<WaitResult> for tiangong_core::browser_trait::WaitResult {
    fn from(r: WaitResult) -> Self {
        Self {
            ok: r.ok,
            condition: r.condition,
            elapsed_ms: r.elapsed_ms,
            error: r.error,
        }
    }
}

impl From<DomRect> for tiangong_core::browser_trait::DomRect {
    fn from(r: DomRect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }
    }
}

impl From<QueryDomElement> for tiangong_core::browser_trait::QueryDomElement {
    fn from(e: QueryDomElement) -> Self {
        Self {
            index: e.index,
            tag: e.tag,
            text: e.text,
            attributes: e.attributes,
            selector: e.selector,
            rect: e.rect.into(),
        }
    }
}

impl From<QueryDomResult> for tiangong_core::browser_trait::QueryDomResult {
    fn from(r: QueryDomResult) -> Self {
        Self {
            selector: r.selector,
            total: r.total,
            returned: r.returned,
            elements: r.elements.into_iter().map(Into::into).collect(),
        }
    }
}

/// 批注矩形区域信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 提取到的元素信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedElement {
    pub tag: String,
    pub text: String,
    pub attributes: HashMap<String, String>,
    pub selector: String,
    pub rect: AnnotationRect,
    pub overlap_ratio: f64,
    pub area: f64,
}

/// 单个批注区域的提取结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationRegionResult {
    pub annotation_index: usize,
    pub rect: AnnotationRect,
    pub elements: Vec<ExtractedElement>,
}

/// 批注元素提取结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationExtractResult {
    pub elements: Vec<AnnotationRegionResult>,
    pub count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_rect_roundtrip() {
        let rect = AnnotationRect {
            x: 10.5,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        let json = serde_json::to_string(&rect).unwrap();
        let back: AnnotationRect = serde_json::from_str(&json).unwrap();
        assert_eq!(back.x, 10.5);
        assert_eq!(back.y, 20.0);
        assert_eq!(back.width, 100.0);
        assert_eq!(back.height, 50.0);
    }

    #[test]
    fn extracted_element_roundtrip() {
        let mut attrs = HashMap::new();
        attrs.insert("id".to_string(), "btn".to_string());
        attrs.insert("class".to_string(), "primary".to_string());

        let el = ExtractedElement {
            tag: "button".to_string(),
            text: "提交".to_string(),
            attributes: attrs,
            selector: "#btn".to_string(),
            rect: AnnotationRect {
                x: 50.0,
                y: 100.0,
                width: 80.0,
                height: 30.0,
            },
            overlap_ratio: 0.85,
            area: 2400.0,
        };
        let json = serde_json::to_string(&el).unwrap();
        let back: ExtractedElement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tag, "button");
        assert_eq!(back.text, "提交");
        assert_eq!(back.selector, "#btn");
        assert_eq!(back.overlap_ratio, 0.85);
        assert_eq!(back.attributes.get("id").unwrap(), "btn");
    }

    #[test]
    fn annotation_region_result_roundtrip() {
        let region = AnnotationRegionResult {
            annotation_index: 0,
            rect: AnnotationRect {
                x: 40.0,
                y: 40.0,
                width: 120.0,
                height: 50.0,
            },
            elements: vec![ExtractedElement {
                tag: "a".to_string(),
                text: "链接".to_string(),
                attributes: HashMap::new(),
                selector: "a[href=\"/test\"]".to_string(),
                rect: AnnotationRect {
                    x: 50.0,
                    y: 50.0,
                    width: 100.0,
                    height: 30.0,
                },
                overlap_ratio: 0.9,
                area: 3000.0,
            }],
        };
        let json = serde_json::to_string(&region).unwrap();
        let back: AnnotationRegionResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.annotation_index, 0);
        assert_eq!(back.elements.len(), 1);
        assert_eq!(back.elements[0].tag, "a");
    }

    #[test]
    fn annotation_extract_result_roundtrip() {
        let result = AnnotationExtractResult {
            elements: vec![AnnotationRegionResult {
                annotation_index: 0,
                rect: AnnotationRect {
                    x: 0.0,
                    y: 0.0,
                    width: 200.0,
                    height: 100.0,
                },
                elements: vec![],
            }],
            count: 1,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: AnnotationExtractResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.count, 1);
        assert_eq!(back.elements.len(), 1);
    }

    #[test]
    fn extracted_element_from_bridge_json() {
        let json = r#"{
            "tag": "input",
            "text": "",
            "attributes": {"name": "email", "type": "email"},
            "selector": "[name=\"email\"]",
            "rect": {"x": 50, "y": 100, "width": 200, "height": 30},
            "overlap_ratio": 0.75,
            "area": 6000
        }"#;
        let el: ExtractedElement = serde_json::from_str(json).unwrap();
        assert_eq!(el.tag, "input");
        assert_eq!(el.attributes.get("name").unwrap(), "email");
        assert_eq!(el.overlap_ratio, 0.75);
    }

    #[test]
    fn plugin_to_core_tab_info_conversion() {
        let tab = BrowserTab {
            id: "tab-1".to_string(),
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
        };
        let core: tiangong_core::browser_trait::TabInfo = tab.into();
        assert_eq!(core.id, "tab-1");
        assert_eq!(core.url, "https://example.com");
        assert_eq!(core.title, "Example");
    }

    #[test]
    fn plugin_to_core_fetch_result_conversion() {
        let resp = BrowserResponse {
            ok: true,
            title: "Test".to_string(),
            content: "Hello".to_string(),
            final_url: "https://example.com".to_string(),
            error: None,
        };
        let core: tiangong_core::browser_trait::FetchResult = resp.into();
        assert!(core.ok);
        assert_eq!(core.title, "Test");
        assert_eq!(core.content, "Hello");
        assert!(core.error.is_none());
    }

    #[test]
    fn plugin_to_core_click_result_conversion() {
        let click = ClickElementResult {
            ok: true,
            error: None,
            wait_result: None,
            candidates: vec![],
            page_diff: None,
        };
        let core: tiangong_core::browser_trait::ClickElementResult = click.into();
        assert!(core.ok);
        assert!(core.error.is_none());
    }

    #[test]
    fn plugin_to_core_fill_result_conversion() {
        let fill = FillFieldResult {
            ok: true,
            strategy: Some("auto".to_string()),
            error: None,
            current_value: Some("test@example.com".to_string()),
            wait_result: None,
            page_diff: None,
        };
        let core: tiangong_core::browser_trait::FillFieldResult = fill.into();
        assert!(core.ok);
        assert_eq!(core.strategy.unwrap(), "auto");
        assert_eq!(core.current_value.unwrap(), "test@example.com");
    }

    #[test]
    fn plugin_to_core_form_extract_conversion() {
        let form = FormExtractResult {
            forms: vec![FormInfo {
                fields: vec![FormField {
                    index: 0,
                    tag: "input".to_string(),
                    field_type: "email".to_string(),
                    name: "email".to_string(),
                    id: "email".to_string(),
                    label: "Email".to_string(),
                    placeholder: "Enter email".to_string(),
                    value: String::new(),
                    required: true,
                    readonly: false,
                    disabled: false,
                    selector: "#email".to_string(),
                    options: vec![],
                }],
                buttons: vec![],
            }],
        };
        let core: tiangong_core::browser_trait::FormExtractResult = form.into();
        assert_eq!(core.forms.len(), 1);
        assert_eq!(core.forms[0].fields.len(), 1);
        assert_eq!(core.forms[0].fields[0].name, "email");
        assert!(core.forms[0].fields[0].required);
    }
}
