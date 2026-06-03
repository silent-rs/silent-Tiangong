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
    ) -> Pin<Box<dyn Future<Output = Option<FillFieldResult>> + Send>> {
        Box::pin(async move { None })
    }

    /// 点击页面元素。
    fn click_element(
        &self,
        _selector: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ClickElementResult>> + Send>> {
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
