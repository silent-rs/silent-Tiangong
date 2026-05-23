//! Memory Turn 记录构建：会话轮次结果提取与持久化
//!
//! 负责从会话消息中提取工具调用、媒体产物、文件产物等 TurnArtifact，
//! 构建 TurnResult 供 Memory Actor 写入长期记忆。
//!
//! 同时提供工具结果的媒体解析、图片归档等辅助能力，
//! 供 ReactEngine 在工具执行完毕后调用。

use std::collections::HashSet;

use crate::session::{Message, MessageRole};

// ── Turn 记忆记录函数 ──

/// 从消息列表中提取所有去重的工具调用名称。
///
/// 仅扫描 `Tool` 角色的消息，通过 `parse_tool_calls_line` 和 `parse_tool_trace_name`
/// 两种格式提取工具名，自动过滤 `recall_memory`，返回去重后的名称列表。
fn extract_turn_tool_calls(messages: &[Message]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for message in messages {
        if message.role != MessageRole::Tool {
            continue;
        }
        let text = message.text_content();
        for name in parse_tool_calls_line(&text)
            .into_iter()
            .chain(parse_tool_trace_name(&text))
        {
            if name == "recall_memory" {
                continue;
            }
            if seen.insert(name.clone()) {
                names.push(name);
            }
        }
    }
    names
}

/// 从消息列表中提取所有媒体、文件、工具结果类型的 TurnArtifact。
///
/// 扫描 `Assistant` 消息中的 media 字段和 `Tool` 消息中的工具追踪内容，
/// 提取图片/视频 URL、写入文件路径、工具结果摘要等产物，
/// 通过 URL/path 去重，最多返回 12 个产物。
fn extract_turn_artifacts(messages: &[Message]) -> Vec<tiangong_memory::TurnArtifact> {
    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();
    for message in messages {
        if message.role == MessageRole::Assistant {
            for block in &message.content {
                if let crate::session::ContentBlock::Media {
                    kind: _,
                    url,
                    title,
                    ..
                } = block
                {
                    let key = format!("media:{url}");
                    if seen.insert(key) {
                        artifacts.push(tiangong_memory::TurnArtifact {
                            kind: tiangong_memory::TurnArtifactKind::Media,
                            tool_name: None,
                            title: title.clone(),
                            url: Some(url.clone()),
                            path: None,
                            summary: None,
                        });
                    }
                }
            }
            continue;
        }

        if message.role != MessageRole::Tool {
            continue;
        }
        let text = message.text_content();
        let tool_name = parse_tool_trace_name(&text);
        for artifact in parse_media_artifacts_from_tool_trace(&text, tool_name.as_deref()) {
            let key = artifact
                .url
                .as_deref()
                .or(artifact.path.as_deref())
                .unwrap_or_default()
                .to_string();
            if !key.is_empty() && seen.insert(key) {
                artifacts.push(artifact);
            }
        }
        if let Some(path) = tool_name
            .as_deref()
            .filter(|name| *name == "write_file" || *name == "replace_in_file")
            .and_then(|_| parse_written_path(&text))
        {
            let key = format!("file:{path}");
            if seen.insert(key) {
                artifacts.push(tiangong_memory::TurnArtifact {
                    kind: tiangong_memory::TurnArtifactKind::File,
                    tool_name: tool_name.clone(),
                    title: Some("文件产物".to_string()),
                    url: None,
                    path: Some(path),
                    summary: parse_summary_line(&text),
                });
            }
        }
        if let Some(tool_name) = tool_name
            && should_record_tool_result(&tool_name)
        {
            let summary =
                parse_summary_line(&text).unwrap_or_else(|| compact_single_memory_text(&text, 240));
            let key = format!("tool:{tool_name}:{summary}");
            if !summary.is_empty() && seen.insert(key) {
                artifacts.push(tiangong_memory::TurnArtifact {
                    kind: tiangong_memory::TurnArtifactKind::ToolResult,
                    tool_name: Some(tool_name),
                    title: Some("工具结果".to_string()),
                    url: None,
                    path: None,
                    summary: Some(summary),
                });
            }
        }
    }
    artifacts.into_iter().take(12).collect()
}

/// 构建轮次摘要文本。
///
/// 优先取最后一条非空 Assistant 消息的压缩内容作为摘要；
/// 如果没有 Assistant 消息，则将所有产物的 URL/path/summary 拼接为摘要。
fn build_turn_memory_summary(
    messages: &[Message],
    artifacts: &[tiangong_memory::TurnArtifact],
) -> String {
    let assistant_summary = messages
        .iter()
        .rev()
        .find(|message| {
            message.role == MessageRole::Assistant && !message.text_content().trim().is_empty()
        })
        .map(|message| compact_single_memory_text(&message.text_content(), 600))
        .unwrap_or_default();
    if !assistant_summary.is_empty() {
        return assistant_summary;
    }
    artifacts
        .iter()
        .filter_map(|artifact| {
            artifact
                .url
                .as_deref()
                .or(artifact.path.as_deref())
                .or(artifact.summary.as_deref())
                .map(str::to_string)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── 解析辅助函数 ──

/// 解析 `tool_calls:` 前缀行，提取逗号分隔的工具调用名称列表。
///
/// 例如 `tool_calls: read_file, write_file` 返回 `["read_file", "write_file"]`。
/// 如果不存在该前缀行，返回空 Vec。
fn parse_tool_calls_line(content: &str) -> Vec<String> {
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix("tool_calls:"))
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// 从工具追踪消息中提取工具名称。
///
/// 匹配格式为 `工具执行 [tool_name]` 的首行，返回方括号内的工具名。
pub(crate) fn parse_tool_trace_name(content: &str) -> Option<String> {
    let first_line = content.lines().next()?.trim();
    let rest = first_line.strip_prefix("工具执行 [")?;
    let end = rest.find(']')?;
    Some(rest[..end].to_string()).filter(|name| !name.is_empty())
}

/// 解析 `summary:` 前缀行，提取单行摘要文本。
///
/// 返回第一个匹配的 summary 行内容（去除前后空白），
/// 如果不存在或为空则返回 None。
fn parse_summary_line(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("summary:")
            .map(str::trim)
            .map(String::from)
            .filter(|item| !item.is_empty())
    })
}

/// 将文本压缩到指定字符数以内。
///
/// 先去除空行并合并为单段文本，如果字符数超过 `max_chars` 则截断并追加 `...`。
/// 用于生成记忆摘要等场景，避免过长文本占用存储空间。
pub(crate) fn compact_single_memory_text(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut clipped = normalized.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}

// ── 媒体产物解析函数 ──

/// 从工具追踪消息中解析媒体类型的 TurnArtifact。
///
/// 逐行扫描内容，提取 Markdown 图片语法 `![title](url)` 和视频 URL 行，
/// 将其转换为 `TurnArtifactKind::Media` 类型的产物条目。
fn parse_media_artifacts_from_tool_trace(
    content: &str,
    tool_name: Option<&str>,
) -> Vec<tiangong_memory::TurnArtifact> {
    let mut artifacts = Vec::new();
    let summary = parse_summary_line(content);
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("![") && trimmed.ends_with(')') {
            let Some(close_alt) = trimmed.find("](") else {
                continue;
            };
            let title = trimmed[2..close_alt].trim();
            let url = trimmed[close_alt + 2..trimmed.len() - 1].trim();
            if url.is_empty() {
                continue;
            }
            artifacts.push(tiangong_memory::TurnArtifact {
                kind: tiangong_memory::TurnArtifactKind::Media,
                tool_name: tool_name.map(String::from),
                title: (!title.is_empty()).then(|| title.to_string()),
                url: Some(url.to_string()),
                path: None,
                summary: summary.clone(),
            });
            continue;
        }

        if let Some(url) = parse_video_url_line(trimmed) {
            artifacts.push(tiangong_memory::TurnArtifact {
                kind: tiangong_memory::TurnArtifactKind::Media,
                tool_name: tool_name.map(String::from),
                title: Some("生成的视频".to_string()),
                url: Some(url),
                path: None,
                summary: summary.clone(),
            });
        }
    }
    artifacts
}

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

/// 将工具结果中的图片 URL 归档到本地存储。
///
/// 仅当工具执行成功且输出可能包含生成的图片时才执行归档，
/// 归档后替换 `result.stdout` 中的远程 URL 为本地路径。
pub(crate) fn localize_tool_result_images(tool_name: &str, result: &mut crate::tool::ToolResult) {
    if !result.ok || !output_may_contain_generated_images(tool_name, &result.stdout) {
        return;
    }
    result.stdout = archive_image_markdown_output(&result.stdout);
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
            match crate::media_archive::archive_image_reference(url, None) {
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

/// 解析工具追踪消息中的写入文件路径。
///
/// 从 `命令:` 前缀行中提取 `path=...` 参数值，
/// 用于记录文件写入操作的目标路径。
fn parse_written_path(content: &str) -> Option<String> {
    let command = content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("命令:")
            .map(str::trim)
            .filter(|item| !item.is_empty())
    })?;
    let rest = command.strip_prefix("path=")?;
    let end = rest.find(" content=").unwrap_or(rest.len());
    Some(rest[..end].trim().to_string()).filter(|path| !path.is_empty())
}

/// 判断工具结果是否应记录为 TurnArtifact。
///
/// 只读类工具（`read_file`、`list_dir`、`tree_dir`、`search_code`、
/// `recall_memory`、`get_skill_detail`）的结果不需要记录，
/// 其他工具的结果均应记录。
fn should_record_tool_result(tool_name: &str) -> bool {
    !matches!(
        tool_name,
        "read_file"
            | "list_dir"
            | "tree_dir"
            | "search_code"
            | "recall_memory"
            | "get_skill_detail"
    )
}

// ── 记忆候选评估 ──

/// 工具执行后生成记忆候选。
///
/// 将工具执行结果结构化记录，由 Memory LLM 在增强版反刍中判断
/// 是否值得记忆以及提取哪些类型（Episode/Entity/Decision/Evidence）。
pub(crate) fn evaluate_tool_result_for_memory(
    tool_name: &str,
    success: bool,
    result_summary: &str,
    file_path: Option<&str>,
    step_index: usize,
) -> Option<tiangong_memory::MemoryCandidate> {
    let summary_trimmed = result_summary.trim();
    if summary_trimmed.is_empty() {
        return None;
    }

    Some(tiangong_memory::MemoryCandidate {
        tool_name: tool_name.to_string(),
        step_index,
        hint: String::new(),
        suggested_kinds: Vec::new(),
        file_path: file_path.map(String::from),
        url: None,
        result_summary: Some(compact_single_memory_text(summary_trimmed, 240)),
        success,
    })
}

/// 构建增强版轮次结果，附加候选列表和对话消息。
pub(crate) fn build_enhanced_memory_turn_result(
    session: &crate::session::Session,
    turn_start_idx: usize,
    user_input: &str,
    candidates: Vec<tiangong_memory::MemoryCandidate>,
) -> tiangong_memory::EnhancedTurnResult {
    let messages = session.messages.get(turn_start_idx..).unwrap_or_default();
    let tool_calls = extract_turn_tool_calls(messages);
    let artifacts = extract_turn_artifacts(messages);
    let summary = build_turn_memory_summary(messages, &artifacts);
    let turn_messages = messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => return None,
                MessageRole::Tool => "tool",
            };
            let content = compact_single_memory_text(&message.text_content(), 400);
            (!content.is_empty()).then(|| tiangong_memory::TurnMessage {
                role: role.to_string(),
                content,
            })
        })
        .collect();

    tiangong_memory::EnhancedTurnResult {
        session_id: session.id.clone(),
        turn_id: scru128::new().to_string(),
        had_tool_calls: !tool_calls.is_empty(),
        user_input: user_input.to_string(),
        summary,
        tool_calls,
        artifacts,
        workspace_id: None,
        memory_candidates: candidates,
        turn_messages,
    }
}
