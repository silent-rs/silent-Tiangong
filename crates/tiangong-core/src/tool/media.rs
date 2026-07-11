//! 工具结果中的媒体产物解析。
//!
//! 图片/视频资源提取（`parse_media_assets_from_tool_result`）供 ReactEngine 在
//! 工具执行后解析工具产出的媒体。工具输出图片的本地化归档由生成插件（如
//! `tiangong-plugin-generate-image`）在产出时自行完成，core 不再参与。

// ── 媒体产物解析函数 ──

/// 从工具结果中解析 MediaAsset 列表（图片或视频）。
///
/// 根据 `tool_name` 判断输出类型：
/// - 如果输出可能包含生成的图片，则解析图片资源；
/// - 如果工具名为 `generate_video`，则解析视频资源；
/// - 其他情况返回空列表。
pub(crate) fn parse_media_assets_from_tool_result(
    tool_name: &str,
    stdout: &str,
    summary: &str,
) -> Vec<tiangong_types::MediaAsset> {
    if output_may_contain_generated_images(tool_name, stdout) {
        return parse_image_assets(stdout);
    }
    if tool_name == "generate_video" {
        parse_video_assets(stdout, summary)
    } else {
        Vec::new()
    }
}

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

/// 从输出文本中解析所有图片类型的 MediaAsset。
///
/// 逐行扫描 Markdown 图片格式 `![title](url)`，
/// 同时识别 data URI 中的 MIME 类型（如 `data:image/png;...`）。
/// 每个匹配行生成一个 `MediaKind::Image` 类型的资源条目。
fn parse_image_assets(output: &str) -> Vec<tiangong_types::MediaAsset> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("![") || !line.ends_with(')') {
                return None;
            }
            let close_alt = line.find("](")?;
            let title = line[2..close_alt].trim();
            let url = line[close_alt + 2..line.len() - 1].trim();
            if url.is_empty() {
                return None;
            }
            let mime_type = url
                .strip_prefix("data:")
                .and_then(|raw| raw.split(';').next())
                .filter(|mime| mime.starts_with("image/"))
                .map(str::to_string);
            Some(tiangong_types::MediaAsset {
                kind: tiangong_types::MediaKind::Image,
                url: url.to_string(),
                mime_type,
                title: (!title.is_empty()).then(|| title.to_string()),
                capability: Some("image_generation".to_string()),
            })
        })
        .collect()
}

/// 从输出文本中解析所有视频类型的 MediaAsset。
///
/// 逐行扫描视频 URL 行，将每个 URL 转换为 `MediaKind::Video` 类型的资源条目，
/// MIME 类型默认为 `video/mp4`。
fn parse_video_assets(output: &str, summary: &str) -> Vec<tiangong_types::MediaAsset> {
    output
        .lines()
        .filter_map(|line| parse_video_url_line(line.trim()))
        .map(|url| tiangong_types::MediaAsset {
            kind: tiangong_types::MediaKind::Video,
            url,
            mime_type: Some("video/mp4".to_string()),
            title: Some(summary.to_string()).filter(|item| !item.trim().is_empty()),
            capability: Some("video_generation".to_string()),
        })
        .collect()
}

/// 解析视频 URL 行。
///
/// 匹配 `Video URL: https://...` 或 `video_url: https://...` 前缀，
/// 提取第一个以 `http://` 或 `https://` 开头的 URL。
fn parse_video_url_line(line: &str) -> Option<String> {
    let raw = line
        .strip_prefix("Video URL:")
        .or_else(|| line.strip_prefix("video_url:"))
        .map(str::trim)?;
    let url = raw.split_whitespace().next().unwrap_or(raw);
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}
