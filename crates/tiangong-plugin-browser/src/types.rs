use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// 浏览器状态变化事件（由浏览器插件产生，App 层路由到活跃对话）
#[derive(Debug, Clone)]
pub enum BrowserEvent {
    /// 页面数据就绪（页面加载/导航完成后的内容推送）
    PageData {
        url: String,
        title: String,
        text: String,
    },
    /// 标签生命周期变化
    TabChanged {
        action: String, // "new" | "switch" | "close" | "update"
        tab_id: String,
        url: Option<String>,
    },
    // 后续扩展预留：
    // /// 页面表单状态变化
    // FormStateChanged { ... },
    // /// 页面元素交互结果
    // ElementInteracted { ... },
    // /// 下载/上传进度
    // TransferProgress { ... },
}

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
        response_tx: oneshot::Sender<FillFieldResult>,
    },
    /// 点击页面元素
    ClickElement {
        selector: String,
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
    /// 智能元素定位（不执行操作，仅查询候选）
    LocateElement {
        query: String,
        response_tx: oneshot::Sender<LocateElementResult>,
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

/// 表单信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormInfo {
    pub fields: Vec<FormField>,
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
    #[serde(rename = "currentValue")]
    pub current_value: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub target: Option<ElementCandidate>,
    #[serde(default)]
    pub candidates: Vec<ElementCandidate>,
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

/// 页面可交互元素
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveElement {
    pub tag: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub href: Option<String>,
}

/// 可交互元素提取结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveElementsResult {
    pub elements: Vec<InteractiveElement>,
    pub count: usize,
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

impl From<FormInfo> for tiangong_core::browser_trait::FormInfo {
    fn from(f: FormInfo) -> Self {
        Self {
            fields: f.fields.into_iter().map(Into::into).collect(),
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

impl From<ElementCandidate> for tiangong_core::browser_trait::ElementCandidate {
    fn from(c: ElementCandidate) -> Self {
        Self {
            selector: c.selector,
            text: c.text,
            tag: c.tag,
            role: c.role,
            label: c.label,
            score: c.score,
            reason: c.reason,
            x: c.x,
            y: c.y,
        }
    }
}

impl From<LocateElementResult> for tiangong_core::browser_trait::LocateElementResult {
    fn from(r: LocateElementResult) -> Self {
        Self {
            ok: r.ok,
            error: r.error,
            ambiguous: r.ambiguous,
            target: r.target.map(Into::into),
            candidates: r.candidates.into_iter().map(Into::into).collect(),
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
            selector: r.selector,
            target: r.target.map(Into::into),
            candidates: r.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ClickElementResult> for tiangong_core::browser_trait::ClickElementResult {
    fn from(r: ClickElementResult) -> Self {
        Self {
            ok: r.ok,
            error: r.error,
            selector: r.selector,
            target: r.target.map(Into::into),
            candidates: r.candidates.into_iter().map(Into::into).collect(),
            x: r.x,
            y: r.y,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> ElementCandidate {
        ElementCandidate {
            selector: "#submit".to_string(),
            text: "提交".to_string(),
            tag: "button".to_string(),
            role: "button".to_string(),
            label: "提交".to_string(),
            score: 96,
            reason: "text match".to_string(),
            x: Some(42),
            y: Some(24),
        }
    }

    #[test]
    fn browser_event_page_data_construct() {
        let event = BrowserEvent::PageData {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            text: "page content".to_string(),
        };
        match event {
            BrowserEvent::PageData { url, title, text } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(title, "Example");
                assert_eq!(text, "page content");
            }
            _ => panic!("expected PageData"),
        }
    }

    #[test]
    fn browser_event_tab_changed_construct() {
        let event = BrowserEvent::TabChanged {
            action: "new".to_string(),
            tab_id: "tab-1".to_string(),
            url: Some("https://example.com".to_string()),
        };
        match event {
            BrowserEvent::TabChanged {
                action,
                tab_id,
                url,
            } => {
                assert_eq!(action, "new");
                assert_eq!(tab_id, "tab-1");
                assert_eq!(url.unwrap(), "https://example.com");
            }
            _ => panic!("expected TabChanged"),
        }
    }

    #[test]
    fn browser_event_clone() {
        let event = BrowserEvent::PageData {
            url: "https://example.com".to_string(),
            title: "Title".to_string(),
            text: "Text".to_string(),
        };
        let cloned = event.clone();
        match cloned {
            BrowserEvent::PageData { url, title, text } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(title, "Title");
                assert_eq!(text, "Text");
            }
            _ => panic!("expected PageData"),
        }
    }

    #[tokio::test]
    async fn event_channel_page_data() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<BrowserEvent>(8);
        tx.send(BrowserEvent::PageData {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            text: "content".to_string(),
        })
        .await
        .unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            BrowserEvent::PageData { url, title, text } => {
                assert_eq!(url, "https://example.com");
                assert_eq!(title, "Example");
                assert_eq!(text, "content");
            }
            _ => panic!("expected PageData"),
        }
    }

    #[tokio::test]
    async fn event_channel_tab_changed() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<BrowserEvent>(8);
        tx.send(BrowserEvent::TabChanged {
            action: "close".to_string(),
            tab_id: "tab-2".to_string(),
            url: None,
        })
        .await
        .unwrap();

        let event = rx.recv().await.unwrap();
        match event {
            BrowserEvent::TabChanged {
                action,
                tab_id,
                url,
            } => {
                assert_eq!(action, "close");
                assert_eq!(tab_id, "tab-2");
                assert!(url.is_none());
            }
            _ => panic!("expected TabChanged"),
        }
    }

    #[tokio::test]
    async fn event_channel_multiple_events() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<BrowserEvent>(32);

        tx.send(BrowserEvent::PageData {
            url: "https://a.com".to_string(),
            title: "A".to_string(),
            text: String::new(),
        })
        .await
        .unwrap();
        tx.send(BrowserEvent::TabChanged {
            action: "new".to_string(),
            tab_id: "t1".to_string(),
            url: Some("https://b.com".to_string()),
        })
        .await
        .unwrap();
        tx.send(BrowserEvent::PageData {
            url: "https://c.com".to_string(),
            title: "C".to_string(),
            text: "content".to_string(),
        })
        .await
        .unwrap();

        let mut count = 0;
        while let Ok(_event) = rx.try_recv() {
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn browser_tab_serde_roundtrip() {
        let tab = BrowserTab {
            id: "tab-1".to_string(),
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
        };
        let json = serde_json::to_string(&tab).unwrap();
        let parsed: BrowserTab = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, tab.id);
        assert_eq!(parsed.url, tab.url);
        assert_eq!(parsed.title, tab.title);
    }

    #[test]
    fn browser_response_into_core_type() {
        let resp = BrowserResponse {
            ok: true,
            title: "Title".to_string(),
            content: "Content".to_string(),
            final_url: "https://example.com".to_string(),
            error: None,
        };
        let core: tiangong_core::browser_trait::FetchResult = resp.into();
        assert!(core.ok);
        assert_eq!(core.title, "Title");
        assert_eq!(core.content, "Content");
    }

    #[test]
    fn browser_tab_into_core_type() {
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
    fn fill_field_result_into_core_type() {
        let result = FillFieldResult {
            ok: true,
            strategy: Some("native".to_string()),
            error: None,
            current_value: Some("filled".to_string()),
            selector: None,
            target: None,
            candidates: Vec::new(),
        };
        let core: tiangong_core::browser_trait::FillFieldResult = result.into();
        assert!(core.ok);
        assert_eq!(core.strategy.unwrap(), "native");
        assert_eq!(core.current_value.unwrap(), "filled");
    }

    #[test]
    fn click_element_result_into_core_type() {
        let result = ClickElementResult {
            ok: false,
            error: Some("not found".to_string()),
            selector: None,
            target: None,
            candidates: Vec::new(),
            x: None,
            y: None,
        };
        let core: tiangong_core::browser_trait::ClickElementResult = result.into();
        assert!(!core.ok);
        assert_eq!(core.error.unwrap(), "not found");
    }

    #[test]
    fn form_extract_result_serde_roundtrip() {
        let result = FormExtractResult {
            forms: vec![FormInfo {
                fields: vec![FormField {
                    index: 0,
                    tag: "input".to_string(),
                    field_type: "text".to_string(),
                    name: "q".to_string(),
                    id: "search".to_string(),
                    label: "Search".to_string(),
                    placeholder: "Type here".to_string(),
                    value: String::new(),
                    required: true,
                    readonly: false,
                    disabled: false,
                    selector: "#search".to_string(),
                    options: vec![],
                }],
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: FormExtractResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.forms.len(), 1);
        assert_eq!(parsed.forms[0].fields.len(), 1);
        assert_eq!(parsed.forms[0].fields[0].name, "q");
        assert!(parsed.forms[0].fields[0].required);
    }

    #[test]
    fn locate_element_result_into_core_type() {
        let result = LocateElementResult {
            ok: true,
            error: None,
            ambiguous: false,
            target: Some(candidate()),
            candidates: vec![candidate()],
        };
        let core: tiangong_core::browser_trait::LocateElementResult = result.into();
        assert!(core.ok);
        assert!(!core.ambiguous);
        assert!(core.target.is_some());
        assert_eq!(core.candidates.len(), 1);
        assert_eq!(core.target.unwrap().selector, "#submit");
    }

    #[test]
    fn locate_element_result_ambiguous_into_core() {
        let result = LocateElementResult {
            ok: false,
            error: Some("多个候选匹配".to_string()),
            ambiguous: true,
            target: None,
            candidates: vec![candidate()],
        };
        let core: tiangong_core::browser_trait::LocateElementResult = result.into();
        assert!(!core.ok);
        assert!(core.ambiguous);
        assert!(core.target.is_none());
        assert_eq!(core.candidates.len(), 1);
    }

    #[test]
    fn fill_result_conversion_keeps_target_and_candidates() {
        let result = FillFieldResult {
            ok: false,
            strategy: None,
            error: Some("找到多个候选元素".to_string()),
            current_value: None,
            selector: None,
            target: None,
            candidates: vec![candidate()],
        };

        let converted: tiangong_core::browser_trait::FillFieldResult = result.into();

        assert_eq!(converted.candidates.len(), 1);
        assert_eq!(converted.candidates[0].selector, "#submit");
        assert_eq!(converted.candidates[0].score, 96);
    }

    #[test]
    fn click_result_conversion_keeps_actual_target() {
        let target = candidate();
        let result = ClickElementResult {
            ok: true,
            error: None,
            selector: Some("#submit".to_string()),
            target: Some(target),
            candidates: Vec::new(),
            x: Some(42),
            y: Some(24),
        };

        let converted: tiangong_core::browser_trait::ClickElementResult = result.into();

        assert_eq!(converted.selector.as_deref(), Some("#submit"));
        assert_eq!(
            converted.target.as_ref().map(|t| t.text.as_str()),
            Some("提交")
        );
        assert_eq!(converted.x, Some(42));
        assert_eq!(converted.y, Some(24));
    }
}
