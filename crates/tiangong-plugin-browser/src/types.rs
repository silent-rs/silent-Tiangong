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
    /// 提取批注区域的元素信息
    AnnotationExtract {
        response_tx: oneshot::Sender<AnnotationExtractResult>,
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
