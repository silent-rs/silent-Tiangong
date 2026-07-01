//! web_fetch 工具规格与覆盖处理器。
//!
//! 从 core 的 tool/web_fetch.rs 整体迁入，改造点：
//! - 参数从位置参数改为命名参数（直接读 call.arguments JSON）
//! - reqwest::blocking 在 spawn_blocking 线程执行（避免污染调用方 async runtime）
//! - trust_mode 仅影响 download 写路径（用 *_with 变体）

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, LOCATION, USER_AGENT};
use reqwest::{StatusCode, Url};
use scraper::{Html, Selector};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool::common as shared;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::plugin::FetchPlugin;

const TOOL_WEB_FETCH: &str = "web_fetch";

const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_CHARS: usize = 12_000;
const MAX_CHARS: usize = 50_000;
const MAX_BODY_BYTES: usize = 1_048_576;
const MAX_DOWNLOAD_BYTES: u64 = 104_857_600;
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchMode {
    Text,
    Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtractMode {
    Auto,
    Text,
    Raw,
}

#[derive(Debug)]
struct WebFetchRequest {
    url: Url,
    mode: FetchMode,
    max_chars: usize,
    output_path: Option<String>,
    overwrite: bool,
    timeout_ms: u64,
    follow_redirects: bool,
    extract_mode: ExtractMode,
}

#[derive(Debug, Serialize)]
struct TextOutput {
    mode: &'static str,
    url: String,
    final_url: String,
    status: u16,
    content_type: String,
    title: Option<String>,
    text: String,
    truncated: bool,
    bytes_read: usize,
}

#[derive(Debug, Serialize)]
struct DownloadOutput {
    mode: &'static str,
    url: String,
    final_url: String,
    status: u16,
    content_type: String,
    file_path: String,
    bytes_written: u64,
    sha256: String,
}

impl FetchPlugin {
    /// 取当前工作目录，未注入时报错。
    fn base(&self) -> Result<PathBuf> {
        self.workspace()
            .ok_or_else(|| anyhow!("会话工作目录未注入，无法执行 web_fetch"))
    }

    pub(crate) fn dispatch(&self, call: &ToolCall) -> Option<ToolResult> {
        match call.name.as_str() {
            TOOL_WEB_FETCH => Some(match self.handle_web_fetch(call) {
                Ok(r) => r,
                Err(e) => tool_error("web_fetch", e),
            }),
            _ => None,
        }
    }

    fn handle_web_fetch(&self, call: &ToolCall) -> Result<ToolResult> {
        let request = WebFetchRequest::from_call(call)?;
        let base = self.base()?;
        let is_full_trust = self.is_full_trust();

        // reqwest::blocking::Client 内部创建 tokio 运行时，在 async 上下文中 drop
        // 会 panic，用独立线程隔离（与原 core 实现一致）。
        std::thread::scope(|scope| {
            scope
                .spawn(move || web_fetch_inner(request, &base, is_full_trust))
                .join()
                .map_err(|_| anyhow!("web_fetch 线程 panic"))?
        })
    }
}

impl WebFetchRequest {
    fn from_call(call: &ToolCall) -> Result<Self> {
        let args = &call.arguments;
        let raw_url = args
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if raw_url.is_empty() {
            return Err(anyhow!("web_fetch 缺少 url 参数"));
        }
        let url = Url::parse(raw_url).with_context(|| format!("URL 格式非法：{raw_url}"))?;
        ensure_url_allowed(&url)?;

        let mode = parse_mode(args.get("mode").and_then(Value::as_str))?;
        let max_chars = parse_usize(args.get("max_chars"), DEFAULT_MAX_CHARS)?.clamp(1, MAX_CHARS);
        let output_path = args
            .get("output_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let overwrite = args
            .get("overwrite")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let timeout_ms =
            parse_u64(args.get("timeout_ms"), DEFAULT_TIMEOUT_MS)?.clamp(1_000, MAX_TIMEOUT_MS);
        let follow_redirects = args
            .get("follow_redirects")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let extract_mode = parse_extract_mode(args.get("extract_mode").and_then(Value::as_str))?;

        if mode == FetchMode::Download && output_path.as_deref().is_some_and(str::is_empty) {
            return Err(anyhow!("download 模式 output_path 不能为空"));
        }

        Ok(Self {
            url,
            mode,
            max_chars,
            output_path,
            overwrite,
            timeout_ms,
            follow_redirects,
            extract_mode,
        })
    }
}

fn web_fetch_inner(
    request: WebFetchRequest,
    base: &Path,
    is_full_trust: bool,
) -> Result<ToolResult> {
    let client = Client::builder()
        .timeout(Duration::from_millis(request.timeout_ms))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("创建 web_fetch HTTP 客户端失败")?;

    let original_url = request.url.clone();
    let response = fetch_with_redirects(&client, request.url.clone(), request.follow_redirects)?;
    let final_url = response.url().clone();
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("HTTP 状态码错误：{status}"));
    }

    match request.mode {
        FetchMode::Text => finish_text_fetch(request, original_url, final_url, status, response),
        FetchMode::Download => finish_download(
            request,
            original_url,
            final_url,
            status,
            response,
            base,
            is_full_trust,
        ),
    }
}

fn finish_text_fetch(
    request: WebFetchRequest,
    original_url: Url,
    final_url: Url,
    status: StatusCode,
    mut response: Response,
) -> Result<ToolResult> {
    let content_type = header_text(response.headers(), CONTENT_TYPE.as_str());
    ensure_text_content_type(&content_type)?;
    let bytes = read_limited(&mut response, MAX_BODY_BYTES)?;
    let bytes_read = bytes.len();
    let raw_text = String::from_utf8(bytes).context("响应体不是有效 UTF-8 文本")?;
    let extracted = extract_text(&raw_text, &content_type, request.extract_mode);
    let truncated = extracted.text.chars().count() > request.max_chars;
    let text = if truncated {
        extracted.text.chars().take(request.max_chars).collect()
    } else {
        extracted.text
    };

    let output = TextOutput {
        mode: "text",
        url: original_url.to_string(),
        final_url: final_url.to_string(),
        status: status.as_u16(),
        content_type,
        title: extracted.title,
        text,
        truncated,
        bytes_read,
    };
    let stdout = serde_json::to_string_pretty(&output).context("序列化 web_fetch 输出失败")?;
    Ok(ToolResult {
        ok: true,
        summary: format!(
            "web_fetch 读取成功：{} status={} bytes={} truncated={}",
            final_url, output.status, output.bytes_read, output.truncated
        ),
        stdout,
        stderr: String::new(),
        exit_code: 0,
        execution: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn finish_download(
    request: WebFetchRequest,
    original_url: Url,
    final_url: Url,
    status: StatusCode,
    mut response: Response,
    base: &Path,
    is_full_trust: bool,
) -> Result<ToolResult> {
    if let Some(length) = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && length > MAX_DOWNLOAD_BYTES
    {
        return Err(anyhow!(
            "下载文件超过大小限制：{} > {}",
            length,
            MAX_DOWNLOAD_BYTES
        ));
    }

    let content_type = header_text(response.headers(), CONTENT_TYPE.as_str());
    let target = resolve_download_target(
        request.output_path.as_deref(),
        &final_url,
        base,
        is_full_trust,
    )?;
    ensure_download_target(&target, request.overwrite)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建下载目录失败：{}", parent.display()))?;
    }

    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("下载目标文件名非法：{}", target.display()))?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("无法确定下载目标父目录：{}", target.display()))?;
    let temp_path = parent.join(format!(".{}.tmp-{}", file_name, scru128::new()));

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .with_context(|| format!("创建下载临时文件失败：{}", temp_path.display()))?;
    let mut hasher = Sha256::new();
    let mut written = 0_u64;
    let mut buf = [0_u8; 16 * 1024];
    loop {
        let n = response
            .read(&mut buf)
            .with_context(|| format!("读取下载响应失败：{}", final_url))?;
        if n == 0 {
            break;
        }
        written = written.saturating_add(n as u64);
        if written > MAX_DOWNLOAD_BYTES {
            let _ = fs::remove_file(&temp_path);
            return Err(anyhow!("下载文件超过大小限制：{}", MAX_DOWNLOAD_BYTES));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .with_context(|| format!("写入下载临时文件失败：{}", temp_path.display()))?;
    }
    file.flush()
        .with_context(|| format!("刷新下载临时文件失败：{}", temp_path.display()))?;

    if request.overwrite && target.exists() {
        fs::remove_file(&target)
            .with_context(|| format!("删除旧下载文件失败：{}", target.display()))?;
    }
    fs::rename(&temp_path, &target).with_context(|| {
        format!(
            "移动下载文件失败：temp={}, target={}",
            temp_path.display(),
            target.display()
        )
    })?;

    let sha256 = format!("{:x}", hasher.finalize());
    let output = DownloadOutput {
        mode: "download",
        url: original_url.to_string(),
        final_url: final_url.to_string(),
        status: status.as_u16(),
        content_type,
        file_path: target.display().to_string(),
        bytes_written: written,
        sha256,
    };
    let stdout = serde_json::to_string_pretty(&output).context("序列化 web_fetch 下载输出失败")?;
    Ok(ToolResult {
        ok: true,
        summary: format!(
            "web_fetch 下载成功：{} ({} bytes, sha256={})",
            shared::display_rel_path_with(&target, base),
            output.bytes_written,
            output.sha256
        ),
        stdout,
        stderr: String::new(),
        exit_code: 0,
        execution: None,
    })
}

fn fetch_with_redirects(client: &Client, mut url: Url, follow_redirects: bool) -> Result<Response> {
    for redirect_count in 0..=MAX_REDIRECTS {
        ensure_url_allowed(&url)?;
        let response = client
            .get(url.clone())
            .header(USER_AGENT, "tiangong-web-fetch/0.1")
            .header(ACCEPT, "*/*")
            .send()
            .with_context(|| format!("请求 URL 失败：{url}"))?;

        if !follow_redirects || !response.status().is_redirection() {
            return Ok(response);
        }
        if redirect_count == MAX_REDIRECTS {
            return Err(anyhow!("重定向次数超过限制：{MAX_REDIRECTS}"));
        }
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| anyhow!("重定向响应缺少 Location：{}", response.status()))?;
        url = url
            .join(location)
            .with_context(|| format!("解析重定向 Location 失败：{location}"))?;
    }
    Err(anyhow!("重定向处理异常"))
}

fn ensure_url_allowed(url: &Url) -> Result<()> {
    match url.scheme() {
        "http" | "https" => {}
        scheme => return Err(anyhow!("协议不允许：{scheme}")),
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("URL 缺少 host：{url}"))?;
    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".localhost") {
        return Err(anyhow!("默认拒绝访问本机地址：{host}"));
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        ensure_public_ip(ip, host)?;
        return Ok(());
    }

    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("无法确定 URL 端口：{url}"))?;
    let addrs = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("解析目标地址失败：{host}"))?;
    let mut resolved = false;
    for addr in addrs {
        resolved = true;
        ensure_public_ip(addr.ip(), host)?;
    }
    if !resolved {
        return Err(anyhow!("解析目标地址为空：{host}"));
    }
    Ok(())
}

fn ensure_public_ip(ip: IpAddr, host: &str) -> Result<()> {
    let denied = match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 224
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
        }
    };
    if denied {
        return Err(anyhow!("默认拒绝访问非公网地址：{host} ({ip})"));
    }
    Ok(())
}

fn read_limited(response: &mut Response, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0_u8; 16 * 1024];
    loop {
        let n = response.read(&mut buf).context("读取响应体失败")?;
        if n == 0 {
            break;
        }
        if out.len().saturating_add(n) > limit {
            return Err(anyhow!("响应体超过大小限制：{limit} bytes"));
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

fn ensure_text_content_type(content_type: &str) -> Result<()> {
    let ct = content_type.to_ascii_lowercase();
    if ct.is_empty()
        || ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("markdown")
        || ct.contains("html")
    {
        return Ok(());
    }
    Err(anyhow!("内容类型不支持 text 模式：{content_type}"))
}

struct ExtractedText {
    title: Option<String>,
    text: String,
}

fn extract_text(raw: &str, content_type: &str, mode: ExtractMode) -> ExtractedText {
    if mode == ExtractMode::Raw || !looks_like_html(content_type, raw) {
        return ExtractedText {
            title: None,
            text: normalize_text(raw),
        };
    }

    let cleaned = remove_html_block(raw, "script");
    let cleaned = remove_html_block(&cleaned, "style");
    let cleaned = remove_html_block(&cleaned, "noscript");
    let cleaned = remove_html_block(&cleaned, "svg");
    let document = Html::parse_document(&cleaned);
    let title = Selector::parse("title").ok().and_then(|selector| {
        document
            .select(&selector)
            .next()
            .map(|node| normalize_text(&node.text().collect::<Vec<_>>().join(" ")))
            .filter(|text| !text.is_empty())
    });
    let text = Selector::parse("body")
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .map(|body| body.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| document.root_element().text().collect::<Vec<_>>().join(" "));

    ExtractedText {
        title,
        text: normalize_text(&text),
    }
}

fn looks_like_html(content_type: &str, raw: &str) -> bool {
    if content_type.to_ascii_lowercase().contains("html") {
        return true;
    }
    raw.trim_start().starts_with("<!doctype html") || raw.trim_start().starts_with("<html")
}

fn normalize_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn remove_html_block(raw: &str, tag: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(start) = lower.find(&open) else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let lower_after_start = after_start.to_ascii_lowercase();
        let Some(end) = lower_after_start.find(&close) else {
            break;
        };
        let skip = end + close.len();
        rest = &after_start[skip..];
    }
    out
}

fn resolve_download_target(
    raw_path: Option<&str>,
    final_url: &Url,
    base: &Path,
    is_full_trust: bool,
) -> Result<PathBuf> {
    let path = raw_path
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| infer_download_file_name(final_url));
    // trust 模式下写路径校验逻辑与非 trust 相同（core 的 trusted 变体实现等价），
    // 统一调用 resolve_write_path_from_base，保留 is_full_trust 参数供将来差异化。
    let _ = is_full_trust;
    shared::resolve_write_path_from_base(&path, base)
}

fn infer_download_file_name(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .map(sanitize_file_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "download.bin".to_string())
}

fn sanitize_file_name(raw: &str) -> String {
    raw.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .collect()
}

fn ensure_download_target(path: &Path, overwrite: bool) -> Result<()> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(anyhow!(
            "下载目标路径不允许包含路径穿越：{}",
            path.display()
        ));
    }
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(anyhow!("下载目标不能是符号链接：{}", path.display()));
        }
        if meta.is_dir() {
            return Err(anyhow!("下载目标不能是目录：{}", path.display()));
        }
        if !overwrite {
            return Err(anyhow!("下载目标文件已存在：{}", path.display()));
        }
    }
    Ok(())
}

fn header_text(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn parse_mode(raw: Option<&str>) -> Result<FetchMode> {
    match raw.unwrap_or("text").trim().to_ascii_lowercase().as_str() {
        "" | "text" => Ok(FetchMode::Text),
        "download" => Ok(FetchMode::Download),
        other => Err(anyhow!("web_fetch mode 非法：{other}")),
    }
}

fn parse_extract_mode(raw: Option<&str>) -> Result<ExtractMode> {
    match raw.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(ExtractMode::Auto),
        "text" => Ok(ExtractMode::Text),
        "raw" => Ok(ExtractMode::Raw),
        other => Err(anyhow!("web_fetch extract_mode 非法：{other}")),
    }
}

fn parse_usize(value: Option<&Value>, default: usize) -> Result<usize> {
    match value {
        Some(Value::Number(n)) => n
            .as_u64()
            .map(|v| v as usize)
            .ok_or_else(|| anyhow!("整数参数非法：{n}")),
        Some(Value::String(s)) => s
            .parse::<usize>()
            .with_context(|| format!("整数参数非法：{s}")),
        _ => Ok(default),
    }
}

fn parse_u64(value: Option<&Value>, default: u64) -> Result<u64> {
    match value {
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| anyhow!("整数参数非法：{n}")),
        Some(Value::String(s)) => s
            .parse::<u64>()
            .with_context(|| format!("整数参数非法：{s}")),
        _ => Ok(default),
    }
}

fn tool_error(tool: &str, e: anyhow::Error) -> ToolResult {
    let summary = format!("{tool} 失败：{e}");
    ToolResult {
        ok: false,
        summary: summary.clone(),
        stdout: String::new(),
        stderr: summary,
        exit_code: 1,
        execution: None,
    }
}

impl ToolSpecProvider for FetchPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: TOOL_WEB_FETCH.to_string(),
            description: "获取 URL 内容。支持 HTTP/HTTPS 网页（text 模式提取正文 / download 模式落盘），含 SSRF 防护。".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "要获取的 HTTP/HTTPS URL" },
                    "mode": { "type": "string", "enum": ["text", "download"], "description": "执行模式，默认 text" },
                    "max_chars": { "type": "integer", "description": "text 模式最多返回字符数，默认 12000，最大 50000", "minimum": 1, "maximum": 50000 },
                    "output_path": { "type": "string", "description": "download 模式目标文件路径，必须位于允许写入目录" },
                    "overwrite": { "type": "boolean", "description": "download 模式是否覆盖已有文件，默认 false" },
                    "timeout_ms": { "type": "integer", "description": "请求超时时间，默认 15000，最大 60000", "minimum": 1000, "maximum": 60000 },
                    "follow_redirects": { "type": "boolean", "description": "是否跟随重定向，默认 true" },
                    "extract_mode": { "type": "string", "enum": ["auto", "text", "raw"], "description": "text 模式提取方式，默认 auto" }
                },
                "required": ["url"]
            }),
        }]
    }
}

impl ToolOverrideHandler for FetchPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let result = self.dispatch(call);
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_core::core::Plugin;

    fn make_plugin(dir: &tempfile::TempDir) -> FetchPlugin {
        let plugin = FetchPlugin::new();
        plugin.set_workspace(dir.path());
        plugin
    }

    fn make_call(args: Value) -> ToolCall {
        ToolCall {
            id: "test".to_string(),
            name: TOOL_WEB_FETCH.to_string(),
            arguments: args,
        }
    }

    #[test]
    fn missing_url_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let result = plugin.dispatch(&make_call(json!({}))).unwrap();
        assert!(!result.ok);
        assert!(result.summary.contains("url"));
    }

    #[test]
    fn localhost_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let result = plugin
            .dispatch(&make_call(json!({ "url": "http://localhost:9999" })))
            .unwrap();
        assert!(!result.ok);
        assert!(result.summary.contains("本机") || result.summary.contains("localhost"));
    }

    #[test]
    fn private_ip_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let result = plugin
            .dispatch(&make_call(json!({ "url": "http://192.168.1.1" })))
            .unwrap();
        assert!(!result.ok);
        assert!(result.summary.contains("非公网") || result.summary.contains("拒绝"));
    }

    #[test]
    fn unknown_tool_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let call = ToolCall {
            id: "test".to_string(),
            name: "not_web_fetch".to_string(),
            arguments: json!({}),
        };
        assert!(plugin.dispatch(&call).is_none());
    }
}
