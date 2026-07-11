//! 媒体归档插件结构体定义与生命周期实现。
//!
//! 接管全部媒体归档职责（用户输入附件归档 + 工具输出图片本地化），
//! 使 core 不再直接依赖 `tiangong-media-archive`。

use tiangong_core::core::Plugin;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

/// 媒体归档插件。
///
/// 通过生命周期钩子接管工具输出的图片本地化：
/// - [`Plugin::on_tool_result_localize`]：工具执行后，把生成图片 Markdown 中
///   的远程 URL 归档为本地路径。
///
/// 输入附件的归档由各入口层在消息投递（deliver）前完成（GUI 的 app_state
/// ingress、Server 的 remote/core.rs），不经过 core。
pub struct MediaArchivePlugin;

impl MediaArchivePlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MediaArchivePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for MediaArchivePlugin {
    fn id(&self) -> &str {
        "media-archive"
    }

    fn on_tool_result_localize(&self, tool_name: &str, stdout: &mut String) {
        if !output_may_contain_generated_images(tool_name, stdout) {
            return;
        }
        *stdout = archive_image_markdown_output(stdout);
    }
}

impl PromptSectionProvider for MediaArchivePlugin {}
impl ToolSpecProvider for MediaArchivePlugin {}
impl ToolOverrideHandler for MediaArchivePlugin {}

// ── 工具输出图片本地化（从 core::tool::media 迁入） ──

/// 判断工具输出是否可能包含生成的图片。
///
/// 满足以下任一条件即返回 true：
/// - 工具名为 `generate_image`；
/// - 输出内容是纯图片 Markdown 格式；
/// - 工具名包含 "image" 且输出包含 `](`（Markdown 图片链接特征）。
fn output_may_contain_generated_images(tool_name: &str, output: &str) -> bool {
    tool_name == "generate_image"
        || looks_like_pure_image_markdown(output)
        || (tool_name.to_ascii_lowercase().contains("image") && output.contains("]("))
}

/// 判断输出是否是纯图片 Markdown 格式。
///
/// 即所有非空行都是 `![alt](url)` 格式时返回 true。
fn looks_like_pure_image_markdown(output: &str) -> bool {
    let trimmed = output.trim();
    !trimmed.is_empty()
        && trimmed.lines().all(|line| {
            let line = line.trim();
            line.is_empty()
                || (line.starts_with("![") && line.contains("](") && line.ends_with(')'))
        })
}

/// 将图片 Markdown 输出中的 URL 归档到本地存储。
///
/// 逐行扫描输出文本，对每行 Markdown 图片调用 `archive_image_reference` 归档，
/// 归档成功则替换为本地路径，失败则保留原始 URL 并打印警告日志。
fn archive_image_markdown_output(output: &str) -> String {
    output
        .lines()
        .map(|line| {
            let Some((alt, url)) = parse_markdown_image_line(line.trim()) else {
                return line.to_string();
            };
            match tiangong_media_archive::archive_image_reference(url, None) {
                Ok(archived) => format!("![{alt}]({})", archived.path()),
                Err(err) => {
                    tracing::warn!(url = %url, error = %err, "图片归档到本地失败，保留原始 URL");
                    line.to_string()
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析单行 Markdown 图片语法，提取 alt 文本和 URL。
///
/// 匹配格式 `![alt](url)`，返回 `(alt, url)` 元组。
/// 如果行不符合图片 Markdown 格式或 URL 为空，返回 None。
fn parse_markdown_image_line(line: &str) -> Option<(&str, &str)> {
    if !line.starts_with("![") || !line.ends_with(')') {
        return None;
    }
    let close_alt = line.find("](")?;
    let alt = line[2..close_alt].trim();
    let url = line[close_alt + 2..line.len() - 1].trim();
    (!url.is_empty()).then_some((alt, url))
}
