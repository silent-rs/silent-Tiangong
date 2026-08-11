//! 浏览器页面获取能力抽象。
//!
//! 原 `tiangong-core::browser_trait`，随能力下沉重构迁入本插件（#225）。
//! `PageFetcher` 的唯一定义方、实现方、调用方都在本插件内：
//! - 定义：本模块
//! - 实现：[`crate::page_fetcher::BrowserPageFetcher`]
//! - 调用：[`crate::page_fetcher::BrowserToolOverride`] + [`crate::watcher::BrowserWatcher`]
//!
//! trait 方法返回类型直接复用 [`crate::types`] 中已有的自有类型
//!（`BrowserPageSnapshot` / `BrowserTab` / `FormExtractResult` 等），
//! 无需额外的转换层。core 不再感知浏览器能力。

use std::future::Future;
use std::pin::Pin;

use crate::types::{
    BrowserPageSnapshot, BrowserResponse, BrowserTab, ClickElementResult, ElementCandidate,
    FillFieldResult, FormExtractResult, LocateElementResult, QueryDomResult,
};

/// 浏览器页面获取能力抽象。
///
/// 返回 `None` 表示能力不可用（如浏览器未就绪），调用方应回退或报错。
pub trait PageFetcher: Send + Sync + 'static {
    /// 获取指定 URL 的页面内容。
    ///
    /// `open` 为 true 时表示用户明确要求打开浏览器（前端会弹出面板），
    /// 为 false 时表示 agent 自主抓取（只亮标记，不弹面板）。
    fn fetch_page(
        &self,
        url: &str,
        max_chars: usize,
        open: bool,
    ) -> Pin<Box<dyn Future<Output = Option<BrowserResponse>> + Send>>;

    /// 获取当前浏览器页面的快照。
    /// 返回 None 表示浏览器未打开或能力不可用。
    fn observe_page(&self) -> Pin<Box<dyn Future<Output = Option<BrowserPageSnapshot>> + Send>>;

    /// 获取标签列表。
    fn list_tabs(&self) -> Pin<Box<dyn Future<Output = Option<Vec<BrowserTab>>> + Send>> {
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

/// 智能定位候选元素的标识文本（供 watcher / handler 复用）。
///
/// 从 [`ElementCandidate`] 提取可读标识，用于快照注入与工具结果摘要。
pub fn candidate_identity(candidate: &ElementCandidate) -> String {
    let mut parts = Vec::new();
    if !candidate.tag.is_empty() {
        parts.push(candidate.tag.clone());
    }
    if !candidate.role.is_empty() {
        parts.push(format!("role={}", candidate.role));
    }
    if !candidate.label.is_empty() {
        parts.push(format!("label=\"{}\"", candidate.label));
    }
    if !candidate.text.is_empty() && candidate.text != candidate.label {
        parts.push(format!("text=\"{}\"", candidate.text));
    }
    if parts.is_empty() {
        "未知元素".to_string()
    } else {
        parts.join(" ")
    }
}
