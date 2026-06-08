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

/// 字段填写结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillFieldResult {
    pub ok: bool,
    pub strategy: Option<String>,
    pub error: Option<String>,
    #[serde(rename = "currentValue")]
    pub current_value: Option<String>,
}

/// 元素点击结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickElementResult {
    pub ok: bool,
    pub error: Option<String>,
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

impl From<FillFieldResult> for tiangong_core::browser_trait::FillFieldResult {
    fn from(r: FillFieldResult) -> Self {
        Self {
            ok: r.ok,
            strategy: r.strategy,
            error: r.error,
            current_value: r.current_value,
        }
    }
}

impl From<ClickElementResult> for tiangong_core::browser_trait::ClickElementResult {
    fn from(r: ClickElementResult) -> Self {
        Self {
            ok: r.ok,
            error: r.error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
