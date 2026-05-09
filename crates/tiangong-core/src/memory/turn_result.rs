//! Memory Turn 记录构建：会话轮次结果提取与持久化
//!
//! 负责从会话消息中提取工具调用、媒体产物、文件产物等 TurnArtifact，
//! 构建 TurnResult 供 Memory Actor 写入长期记忆。
//!
//! TODO: Phase 2 后续步骤中将 core/mod.rs 中的旧实现迁移到此处。

#![allow(dead_code)]

use std::collections::HashSet;

use crate::session::{Message, MessageRole};

/// 构建 TurnResult，记录一轮对话的关键产物和工具调用。
pub(crate) fn build_memory_turn_result(
    session: &crate::session::Session,
    turn_start_idx: usize,
    user_input: &str,
) -> tiangong_memory::TurnResult {
    let messages = session.messages.get(turn_start_idx..).unwrap_or_default();
    let tool_calls = extract_turn_tool_calls(messages);
    let artifacts = extract_turn_artifacts(messages);
    let summary = build_turn_memory_summary(messages, &artifacts);
    tiangong_memory::TurnResult {
        session_id: session.id.clone(),
        turn_id: scru128::new().to_string(),
        had_tool_calls: !tool_calls.is_empty(),
        user_input: user_input.to_string(),
        summary,
        tool_calls,
        artifacts,
        workspace_id: None,
    }
}

fn extract_turn_tool_calls(messages: &[Message]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for message in messages {
        if message.role != MessageRole::Tool {
            continue;
        }
        for name in parse_tool_calls_line(&message.content)
            .into_iter()
            .chain(parse_tool_trace_name(&message.content))
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

fn extract_turn_artifacts(messages: &[Message]) -> Vec<tiangong_memory::TurnArtifact> {
    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();
    for message in messages {
        if message.role == MessageRole::Assistant {
            for media in &message.media {
                let key = format!("media:{}", media.url);
                if seen.insert(key) {
                    artifacts.push(tiangong_memory::TurnArtifact {
                        kind: tiangong_memory::TurnArtifactKind::Media,
                        tool_name: None,
                        title: media.title.clone(),
                        url: Some(media.url.clone()),
                        path: None,
                        summary: media.capability.clone(),
                    });
                }
            }
            continue;
        }

        if message.role != MessageRole::Tool {
            continue;
        }
        let tool_name = parse_tool_trace_name(&message.content);
        for artifact in
            parse_media_artifacts_from_tool_trace(&message.content, tool_name.as_deref())
        {
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
            .and_then(|_| parse_written_path(&message.content))
        {
            let key = format!("file:{path}");
            if seen.insert(key) {
                artifacts.push(tiangong_memory::TurnArtifact {
                    kind: tiangong_memory::TurnArtifactKind::File,
                    tool_name: tool_name.clone(),
                    title: Some("文件产物".to_string()),
                    url: None,
                    path: Some(path),
                    summary: parse_summary_line(&message.content),
                });
            }
        }
        if let Some(tool_name) = tool_name
            && should_record_tool_result(&tool_name)
        {
            let summary = parse_summary_line(&message.content)
                .unwrap_or_else(|| compact_single_memory_text(&message.content, 240));
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

fn build_turn_memory_summary(
    messages: &[Message],
    artifacts: &[tiangong_memory::TurnArtifact],
) -> String {
    let assistant_summary = messages
        .iter()
        .rev()
        .find(|message| {
            message.role == MessageRole::Assistant && !message.content.trim().is_empty()
        })
        .map(|message| compact_single_memory_text(&message.content, 600))
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

pub(crate) fn parse_tool_trace_name(content: &str) -> Option<String> {
    let first_line = content.lines().next()?.trim();
    let rest = first_line.strip_prefix("工具执行 [")?;
    let end = rest.find(']')?;
    Some(rest[..end].to_string()).filter(|name| !name.is_empty())
}

fn parse_summary_line(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("summary:")
            .map(str::trim)
            .map(String::from)
            .filter(|item| !item.is_empty())
    })
}

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

fn parse_video_url_line(line: &str) -> Option<String> {
    let raw = line
        .strip_prefix("Video URL:")
        .or_else(|| line.strip_prefix("video_url:"))
        .map(str::trim)?;
    let url = raw.split_whitespace().next().unwrap_or(raw);
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}
