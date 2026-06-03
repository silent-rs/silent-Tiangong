use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

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
