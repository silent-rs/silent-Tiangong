//! web_fetch 工具链路操作（web_fetch / set_workspace）。

use serde::{Deserialize, Serialize};

use crate::{Ack, ExtractMode, FetchMode, FetchOperation};

pub const WEB_FETCH_OPERATION: &str = "fetch.web_fetch";
pub const SET_WORKSPACE_OPERATION: &str = "fetch.set_workspace";

/// `web_fetch` 工具请求（与原进程内插件参数对齐，但改为可选字符串、由 sidecar 解析 URL/SSRF）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebFetchRequest {
    /// 要获取的 HTTP/HTTPS URL（必填）。
    pub url: String,
    /// 执行模式，默认 text。
    #[serde(default)]
    pub mode: FetchMode,
    /// text 模式最多返回字符数，默认 12000，最大 50000。
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// download 模式目标文件路径（相对工作目录）。
    #[serde(default)]
    pub output_path: Option<String>,
    /// download 模式是否覆盖已有文件，默认 false。
    #[serde(default)]
    pub overwrite: bool,
    /// 请求超时毫秒，默认 15000，最大 60000。
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// 是否跟随重定向，默认 true。
    #[serde(default = "default_follow_redirects")]
    pub follow_redirects: bool,
    /// text 模式提取方式，默认 auto。
    #[serde(default)]
    pub extract_mode: ExtractMode,
}

fn default_max_chars() -> usize {
    12_000
}
fn default_timeout_ms() -> u64 {
    15_000
}
fn default_follow_redirects() -> bool {
    true
}

/// `web_fetch` 工具响应：保留与 core `ToolResult` 同构字段，便于 sidecar 直接构造。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebFetchResponse {
    pub ok: bool,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub struct WebFetch;
impl FetchOperation for WebFetch {
    const NAME: &'static str = WEB_FETCH_OPERATION;
    type Request = WebFetchRequest;
    type Response = WebFetchResponse;
}

/// `set_workspace` 钩子请求：通知 sidecar 工作区变更（download 落盘基准）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetWorkspaceRequest {
    /// 新工作目录；None 表示清除。
    #[serde(default)]
    pub workspace: Option<String>,
}

pub struct SetWorkspace;
impl FetchOperation for SetWorkspace {
    const NAME: &'static str = SET_WORKSPACE_OPERATION;
    type Request = SetWorkspaceRequest;
    type Response = Ack;
}
