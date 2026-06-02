use std::future::Future;
use std::pin::Pin;

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
