//! 记忆轮次提取：从会话消息构建增强反刍结果。
//!
//! 本模块整体迁自 `tiangong-core/src/memory/turn_result.rs` 的记忆评估函数群
//!（候选评估 + 产物提取 + 摘要构建），是 memory 插件 `on_turn_finished` 的
//! 核心依赖。搬迁后 core 的 turn_result.rs 只保留媒体/图片归档相关函数。
//!
//! 入口函数 [`build_turn_memory_result`] 封装全部逻辑：从 `&Session` +
//! `turn_start_idx` 重建 `EnhancedTurnResult`（含候选、产物、消息），
//! 供插件调 `handle.run_enhanced_micro_rumination_blocking(result)`。

use std::collections::HashSet;

use tiangong_core::session::{Message, MessageRole, Session};
use tiangong_memory::{EnhancedTurnResult, MemoryCandidate, TurnArtifact, TurnArtifactKind};

/// 从会话消息构建增强版轮次结果，并评估本轮所有工具结果生成记忆候选。
///
/// 供 memory 插件在 `on_turn_finished` 钩子中调用：传入 `&Session` + `turn_start_idx`
/// + 用户输入，返回完整的 `EnhancedTurnResult`（含候选、产物、消息）。
///
/// 插件拿到结果后调 `handle.run_enhanced_micro_rumination_blocking(result)`。
///
/// 候选评估从 session 的 tool result 消息回溯（替代原 ReactEngine 内部的
/// `submit_memory_candidate` 逐条提交），确保插件无需感知 engine 内部状态。
pub(super) fn build_turn_memory_result(
    session: &Session,
    turn_start_idx: usize,
    user_input: &str,
) -> EnhancedTurnResult {
    let messages = session.messages.get(turn_start_idx..).unwrap_or_default();
    let mut candidates = Vec::new();
    let mut step_index = 0usize;
    let mut path_by_call_id: std::collections::HashMap<&str, &str> =
        std::collections::HashMap::new();
    for msg in messages {
        if msg.role == MessageRole::Assistant {
            for tc in &msg.tool_calls {
                if let Some(path) = tc.arguments.get("path").and_then(|v| v.as_str()) {
                    path_by_call_id.insert(tc.id.as_str(), path);
                }
            }
        }
    }
    for msg in messages {
        if msg.role != MessageRole::Tool {
            continue;
        }
        let tool_name = msg.tool_name.as_deref().unwrap_or("");
        let success = !msg.tool_result_is_error;
        let summary = msg.text_content();
        let file_path = path_by_call_id
            .get(msg.tool_call_id.as_deref().unwrap_or_default())
            .copied();
        if let Some(candidate) =
            evaluate_tool_result_for_memory(tool_name, success, &summary, file_path, step_index)
        {
            candidates.push(candidate);
            step_index += 1;
        }
    }

    build_enhanced_memory_turn_result(session, turn_start_idx, user_input, candidates)
}

fn build_enhanced_memory_turn_result(
    session: &Session,
    turn_start_idx: usize,
    user_input: &str,
    candidates: Vec<MemoryCandidate>,
) -> EnhancedTurnResult {
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

    EnhancedTurnResult {
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

fn evaluate_tool_result_for_memory(
    tool_name: &str,
    success: bool,
    result_summary: &str,
    file_path: Option<&str>,
    step_index: usize,
) -> Option<MemoryCandidate> {
    let summary_trimmed = result_summary.trim();
    if summary_trimmed.is_empty() {
        return None;
    }
    Some(MemoryCandidate {
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

fn extract_turn_artifacts(messages: &[Message]) -> Vec<TurnArtifact> {
    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();
    for message in messages {
        if message.role == MessageRole::Assistant {
            for block in &message.content {
                if let tiangong_core::session::ContentBlock::Media {
                    kind: _,
                    url,
                    title,
                    ..
                } = block
                {
                    let key = format!("media:{url}");
                    if seen.insert(key) {
                        artifacts.push(TurnArtifact {
                            kind: TurnArtifactKind::Media,
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
                artifacts.push(TurnArtifact {
                    kind: TurnArtifactKind::File,
                    tool_name: tool_name.clone(),
                    title: Some("文件产物".to_string()),
                    url: None,
                    path: Some(path),
                    summary: parse_summary_line(&text),
                });
            }
        }
        if let Some(tool_name) = tool_name {
            if should_record_tool_result(&tool_name) {
                let summary = parse_summary_line(&text)
                    .unwrap_or_else(|| compact_single_memory_text(&text, 240));
                let key = format!("tool:{tool_name}:{summary}");
                if !summary.is_empty() && seen.insert(key) {
                    artifacts.push(TurnArtifact {
                        kind: TurnArtifactKind::ToolResult,
                        tool_name: Some(tool_name),
                        title: Some("工具结果".to_string()),
                        url: None,
                        path: None,
                        summary: Some(summary),
                    });
                }
            }
        }
    }
    artifacts.into_iter().take(12).collect()
}

fn build_turn_memory_summary(messages: &[Message], artifacts: &[TurnArtifact]) -> String {
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

fn parse_tool_trace_name(content: &str) -> Option<String> {
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

fn compact_single_memory_text(text: &str, max_chars: usize) -> String {
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

fn parse_media_artifacts_from_tool_trace(
    content: &str,
    tool_name: Option<&str>,
) -> Vec<TurnArtifact> {
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
            artifacts.push(TurnArtifact {
                kind: TurnArtifactKind::Media,
                tool_name: tool_name.map(String::from),
                title: (!title.is_empty()).then(|| title.to_string()),
                url: Some(url.to_string()),
                path: None,
                summary: summary.clone(),
            });
            continue;
        }
        if let Some(url) = parse_video_url_line(trimmed) {
            artifacts.push(TurnArtifact {
                kind: TurnArtifactKind::Media,
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

/// 解析视频 URL 行。core 的 turn_result.rs 也有一份（媒体归档群用），保持同步。
fn parse_video_url_line(line: &str) -> Option<String> {
    let raw = line
        .strip_prefix("Video URL:")
        .or_else(|| line.strip_prefix("video_url:"))
        .map(str::trim)?;
    let url = raw.split_whitespace().next().unwrap_or(raw);
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

fn parse_written_path(content: &str) -> Option<String> {
    let command = content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("命令:")
            .map(str::trim)
            .filter(|s| !s.is_empty())
    })?;
    command
        .split_whitespace()
        .find_map(|part| part.strip_prefix("path=").map(str::to_string))
        .filter(|path| !path.is_empty())
}

fn should_record_tool_result(tool_name: &str) -> bool {
    !matches!(
        tool_name,
        "list_dir"
            | "tree_dir"
            | "current_time"
            | "index_search"
            | "search_code"
            | "recall_memory"
            | "get_skill_detail"
    )
}
