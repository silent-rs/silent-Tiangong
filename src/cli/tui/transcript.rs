use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::core::session::{Message, MessageRole};

pub(super) fn format_transcript(
    messages: &[Message],
    content_width: u16,
    has_pending_turn: bool,
) -> Text<'static> {
    if messages.is_empty() {
        return Text::raw("开始新对话吧。");
    }

    let pending_assistant_idx = active_assistant_message_index(messages, has_pending_turn);
    let mut lines = Vec::new();
    for (idx, message) in messages.iter().enumerate() {
        match message.role {
            MessageRole::User => append_user_message_lines(&mut lines, message),
            MessageRole::Assistant => {
                let collapse_thinking = pending_assistant_idx != Some(idx);
                append_assistant_message_lines(
                    &mut lines,
                    message,
                    content_width,
                    collapse_thinking,
                );
            }
            MessageRole::System => append_system_message_lines(&mut lines, message),
        }
        lines.push(Line::default());
    }
    Text::from(lines)
}

#[derive(Debug, Clone)]
struct SystemEvent {
    title: String,
    details: Vec<String>,
}

fn append_user_message_lines(target: &mut Vec<Line<'static>>, message: &Message) {
    let title_style = Style::default()
        .fg(Color::LightCyan)
        .add_modifier(Modifier::BOLD);
    target.push(build_event_title_line("你", title_style));

    let content = message.content.trim_end();
    let display_content = if content.trim().is_empty() {
        "..."
    } else {
        content
    };
    append_markdown_lines_with_prefix(target, display_content, "  ");
}

fn append_assistant_message_lines(
    target: &mut Vec<Line<'static>>,
    message: &Message,
    content_width: u16,
    collapse_thinking: bool,
) {
    let title_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    target.push(build_event_title_line("天工", title_style));

    let content = message.content.trim_end();
    let thinking = message.reasoning_content.trim_end();
    let thinking_style = Style::default()
        .fg(Color::Rgb(196, 214, 235))
        .add_modifier(Modifier::ITALIC);

    if !thinking.trim().is_empty() {
        if collapse_thinking {
            target.push(build_collapsed_thinking_line(thinking, thinking_style));
        } else {
            target.push(build_thinking_block_line(
                "  [思考]",
                thinking_style,
                content_width,
            ));
            append_thinking_markdown_block(target, thinking, thinking_style, content_width);
            target.push(build_thinking_block_line(
                "  [/思考]",
                thinking_style,
                content_width,
            ));
        }
    }

    if content.trim().is_empty() {
        if thinking.trim().is_empty() {
            target.push(build_event_detail_line(
                "...",
                Style::default().fg(Color::Gray),
            ));
        }
    } else {
        append_markdown_lines_with_prefix(target, content, "  ");
    }
}

fn active_assistant_message_index(messages: &[Message], has_pending_turn: bool) -> Option<usize> {
    if !has_pending_turn {
        return None;
    }
    messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, message)| (message.role == MessageRole::Assistant).then_some(idx))
}

fn build_collapsed_thinking_line(thinking: &str, style: Style) -> Line<'static> {
    let line_count = thinking.lines().count();
    let char_count = thinking.chars().count();
    let preview = thinking
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| truncate_chars(line.trim(), 72))
        .unwrap_or_default();
    let summary = if preview.is_empty() {
        format!("  [思考已收起：{line_count} 行，{char_count} 字]")
    } else {
        format!("  [思考已收起：{line_count} 行，{char_count} 字] {preview}")
    };
    Line::from(Span::styled(summary, style))
}

fn append_system_message_lines(target: &mut Vec<Line<'static>>, message: &Message) {
    if let Some((title, detail_markdown)) = parse_tool_event_markdown(message.content.as_str()) {
        let write_file_compacted = if title.contains("write_file") {
            collapse_write_file_command_region(detail_markdown.as_str())
        } else {
            detail_markdown
        };
        let collapsed_detail_markdown =
            collapse_tool_output_blocks(write_file_compacted.as_str(), 5);
        let title_style = Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD);
        target.push(build_event_title_line(title.as_str(), title_style));
        if collapsed_detail_markdown.trim().is_empty() {
            target.push(build_event_detail_line(
                "无输出",
                Style::default().fg(Color::Gray),
            ));
        } else {
            append_markdown_lines_with_prefix(target, collapsed_detail_markdown.as_str(), "  ");
        }
        return;
    }

    if let Some((stage, title, detail_markdown)) =
        parse_llm_event_markdown(message.content.as_str())
    {
        let title_style = Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD);
        target.push(build_event_title_line(title.as_str(), title_style));

        // 流式阶段判定：reasoning_content 非空 或 body 不含最终格式化标记（reasoning:/content:/tool_calls:）
        let streaming_reasoning = message.reasoning_content.trim();
        let is_streaming =
            !streaming_reasoning.is_empty() || is_stage_thinking_in_progress(&detail_markdown);
        if is_streaming {
            let thinking_style = Style::default()
                .fg(Color::Rgb(196, 214, 235))
                .add_modifier(Modifier::ITALIC);
            if !streaming_reasoning.is_empty() {
                append_styled_markdown_lines_with_prefix(
                    target,
                    streaming_reasoning,
                    "  ",
                    thinking_style,
                );
            }
            // 如果还有 content 部分（流式 content delta），也展示
            let streaming_content = detail_markdown.trim();
            if !streaming_content.is_empty() {
                append_markdown_lines_with_prefix(target, streaming_content, "  ");
            }
            if streaming_reasoning.is_empty() && detail_markdown.trim().is_empty() {
                target.push(build_event_detail_line(
                    "...",
                    Style::default().fg(Color::Gray),
                ));
            }
            return;
        }

        // 最终态：使用已有的折叠逻辑
        let collapsed_reasoning = collapse_llm_reasoning_block(detail_markdown.as_str());
        let collapsed_detail_markdown = if stage == "planning-agent" {
            collapse_planning_content_block(collapsed_reasoning.as_str())
        } else {
            collapsed_reasoning
        };
        if collapsed_detail_markdown.trim().is_empty() {
            target.push(build_event_detail_line(
                "无输出",
                Style::default().fg(Color::Gray),
            ));
        } else {
            append_markdown_lines_with_prefix(target, collapsed_detail_markdown.as_str(), "  ");
        }
        return;
    }

    if let Some(detail_markdown) = parse_plan_execution_summary_markdown(message.content.as_str()) {
        let compact_detail_markdown = collapse_plan_execution_summary_markdown(&detail_markdown);
        let title_style = Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD);
        target.push(build_event_title_line("Plan 执行总结", title_style));
        if compact_detail_markdown.trim().is_empty() {
            target.push(build_event_detail_line(
                "无输出",
                Style::default().fg(Color::Gray),
            ));
        } else {
            append_markdown_lines_with_prefix(target, compact_detail_markdown.as_str(), "  ");
        }
        return;
    }

    let event = parse_system_event(message.content.as_str());
    let title_style = Style::default()
        .fg(Color::LightGreen)
        .add_modifier(Modifier::BOLD);
    target.push(build_event_title_line(event.title.as_str(), title_style));

    let detail_style = Style::default().fg(Color::Gray);
    if event.details.is_empty() {
        target.push(build_event_detail_line("无摘要", detail_style));
        return;
    }
    for detail in event.details {
        target.push(build_event_detail_line(detail.as_str(), detail_style));
    }
}

fn parse_system_event(content: &str) -> SystemEvent {
    let text = content.trim();
    if text.is_empty() {
        return SystemEvent {
            title: "系统事件".to_string(),
            details: vec!["无内容".to_string()],
        };
    }

    if let Some(stage) = text.lines().next().and_then(parse_llm_stage) {
        let mut details = Vec::new();
        if let Some(tool_calls) =
            extract_prefixed_line_value(text, "tool_calls:").filter(|value| !value.is_empty())
        {
            details.push(format!("工具调用：{tool_calls}"));
        }
        if let Some(preview) = extract_llm_content_preview(text) {
            details.push(format!("摘要：{preview}"));
        }
        return SystemEvent {
            title: format_stage_title(&stage),
            details,
        };
    }

    let summary = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| truncate_chars(line.trim(), 140))
        .unwrap_or_else(|| "无内容".to_string());
    SystemEvent {
        title: "系统".to_string(),
        details: vec![summary],
    }
}

fn parse_tool_event_markdown(content: &str) -> Option<(String, String)> {
    let mut lines = content.lines();
    let header = lines.next()?.trim();
    if !header.starts_with("工具执行 [") {
        return None;
    }

    let start = header.find('[')? + 1;
    let rest = &header[start..];
    let end = rest.find(']')?;
    let tool_name = &rest[..end];
    let body = lines.collect::<Vec<_>>().join("\n");
    Some((format_tool_event_title(tool_name, &body), body))
}

fn parse_llm_event_markdown(content: &str) -> Option<(String, String, String)> {
    let mut lines = content.lines();
    let header = lines.next()?.trim();
    let stage = parse_llm_stage(header)?;
    let body = lines.collect::<Vec<_>>().join("\n");
    Some((stage.clone(), format_stage_title(&stage), body))
}

fn collapse_tool_output_blocks(detail_markdown: &str, max_lines: usize) -> String {
    let source_lines = detail_markdown.lines().collect::<Vec<_>>();
    if source_lines.is_empty() {
        return String::new();
    }

    let mut out = Vec::new();
    let mut idx = 0usize;
    while idx < source_lines.len() {
        let line = source_lines[idx];
        let tag = line.trim();
        if tag != "stdout:" && tag != "stderr:" {
            out.push(line.to_string());
            idx += 1;
            continue;
        }

        out.push(line.to_string());
        idx += 1;
        if idx >= source_lines.len() {
            break;
        }
        if !source_lines[idx].trim().starts_with("```") {
            continue;
        }

        let fence = source_lines[idx].to_string();
        out.push(fence);
        idx += 1;

        let start = idx;
        while idx < source_lines.len() {
            if source_lines[idx].trim().starts_with("```") {
                break;
            }
            idx += 1;
        }
        let block_lines = &source_lines[start..idx];
        if block_lines.len() <= max_lines {
            out.extend(block_lines.iter().map(|line| line.to_string()));
        } else {
            out.extend(
                block_lines
                    .iter()
                    .take(max_lines)
                    .map(|line| line.to_string()),
            );
            out.push(format!("...(省略 {} 行)", block_lines.len() - max_lines));
        }
        if idx < source_lines.len() {
            out.push(source_lines[idx].to_string());
            idx += 1;
        }
    }
    out.join("\n")
}

fn collapse_write_file_command_region(detail_markdown: &str) -> String {
    let source_lines = detail_markdown.lines().collect::<Vec<_>>();
    if source_lines.is_empty() {
        return String::new();
    }
    let Some(cmd_idx) = source_lines
        .iter()
        .position(|line| line.starts_with("命令:"))
    else {
        return detail_markdown.to_string();
    };

    let end_idx = source_lines
        .iter()
        .enumerate()
        .skip(cmd_idx + 1)
        .find_map(|(idx, line)| {
            let trimmed = line.trim_start();
            (trimmed.starts_with("ok=")
                || trimmed.starts_with("summary:")
                || trimmed.starts_with("stdout:")
                || trimmed.starts_with("stderr:")
                || trimmed.starts_with("```"))
            .then_some(idx)
        })
        .unwrap_or(source_lines.len());

    let command_region = source_lines[cmd_idx..end_idx].join("\n");
    if command_region.contains("content=...(") && end_idx == cmd_idx + 1 {
        return detail_markdown.to_string();
    }

    let first = source_lines[cmd_idx]
        .trim_start_matches("命令:")
        .trim()
        .to_string();
    let path = first
        .split_whitespace()
        .next()
        .map(|item| truncate_chars(item, 120))
        .unwrap_or_default();
    let replacement = if path.is_empty() {
        "命令: content=...".to_string()
    } else {
        format!("命令: path={path} content=...")
    };

    let mut out = Vec::new();
    out.extend(
        source_lines
            .iter()
            .take(cmd_idx)
            .map(|line| line.to_string()),
    );
    out.push(replacement);
    out.extend(
        source_lines
            .iter()
            .skip(end_idx)
            .map(|line| line.to_string()),
    );
    out.join("\n")
}

fn collapse_llm_reasoning_block(detail_markdown: &str) -> String {
    let source_lines = detail_markdown.lines().collect::<Vec<_>>();
    if source_lines.is_empty() {
        return String::new();
    }

    let mut out = Vec::new();
    let mut idx = 0usize;
    while idx < source_lines.len() {
        let line = source_lines[idx];
        if line.trim() != "reasoning:" {
            out.push(line.to_string());
            idx += 1;
            continue;
        }

        out.push(line.to_string());
        idx += 1;
        let start = idx;
        while idx < source_lines.len() {
            if source_lines[idx].trim() == "content:" {
                break;
            }
            idx += 1;
        }
        let reasoning_lines = &source_lines[start..idx];
        let reasoning_text = reasoning_lines.join("\n");
        let line_count = reasoning_lines.len();
        let char_count = reasoning_text.chars().count();
        let preview = reasoning_lines
            .iter()
            .find_map(|item| {
                let trimmed = item.trim();
                (!trimmed.is_empty()).then_some(trimmed)
            })
            .map(|text| truncate_chars(text, 72))
            .unwrap_or_default();
        if preview.is_empty() {
            out.push(format!("[思考已收起：{line_count} 行，{char_count} 字]"));
        } else {
            out.push(format!(
                "[思考已收起：{line_count} 行，{char_count} 字] {preview}"
            ));
        }
    }
    out.join("\n")
}

fn collapse_planning_content_block(detail_markdown: &str) -> String {
    let source_lines = detail_markdown.lines().collect::<Vec<_>>();
    if source_lines.is_empty() {
        return String::new();
    }

    let Some(content_idx) = source_lines
        .iter()
        .position(|line| line.trim() == "content:")
    else {
        return detail_markdown.to_string();
    };

    let content_lines = &source_lines[content_idx + 1..];
    let content_text = content_lines.join("\n").trim().to_string();

    let mut out = source_lines
        .iter()
        .take(content_idx + 1)
        .map(|line| line.to_string())
        .collect::<Vec<_>>();

    if content_text.is_empty() {
        out.push("[计划内容为空]".to_string());
        return out.join("\n");
    }

    let line_count = content_lines.len();
    let char_count = content_text.chars().count();
    let preview = summarize_planning_content_preview(&content_text);
    if preview.is_empty() {
        out.push(format!(
            "[计划内容已收起：{line_count} 行，{char_count} 字]"
        ));
    } else {
        out.push(format!(
            "[计划内容已收起：{line_count} 行，{char_count} 字] {preview}"
        ));
    }
    out.join("\n")
}

fn summarize_planning_content_preview(content: &str) -> String {
    if let Some(parsed) = parse_planning_json(content) {
        let objective = parsed
            .get("objective")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("目标={}", truncate_chars(value, 28)));
        let summary = parsed
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("摘要={}", truncate_chars(value, 32)));
        let plans = parsed
            .get("plans")
            .and_then(Value::as_array)
            .map(|items| items.as_slice())
            .unwrap_or(&[]);
        let plan_count = plans.len();
        let step_count = plans
            .iter()
            .map(|plan| {
                plan.get("execution_steps")
                    .and_then(Value::as_array)
                    .map(|steps| steps.len())
                    .unwrap_or(0)
            })
            .sum::<usize>();
        let risk_count = parsed
            .get("risks")
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0);

        let mut parts = Vec::new();
        if let Some(objective) = objective {
            parts.push(objective);
        }
        if let Some(summary) = summary {
            parts.push(summary);
        }
        if plan_count > 0 {
            parts.push(format!("plans={plan_count}"));
        }
        if step_count > 0 {
            parts.push(format!("steps={step_count}"));
        }
        if risk_count > 0 {
            parts.push(format!("risks={risk_count}"));
        }
        if !parts.is_empty() {
            return parts.join("，");
        }
    }

    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| truncate_chars(line, 72))
        .unwrap_or_default()
}

fn parse_planning_json(content: &str) -> Option<Value> {
    let trimmed = content.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && value.is_object()
    {
        return Some(value);
    }

    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end])
        .ok()
        .filter(Value::is_object)
}

/// 判断 LLM 输出系统消息是否处于流式阶段（尚未被最终格式化内容替换）。
/// 最终格式化内容由 `format_llm_output_message` 生成，包含 `reasoning:` / `content:` / `tool_calls:` 标记。
/// 如果 body 不含这些标记，说明还在流式追加中。
fn is_stage_thinking_in_progress(detail_markdown: &str) -> bool {
    let trimmed = detail_markdown.trim();
    if trimmed.is_empty() {
        // body 为空表示刚创建，还在等待第一个 content delta
        return true;
    }
    // 最终格式化内容至少会包含 "reasoning:" 或 "content:" 标记之一
    !trimmed.contains("reasoning:") && !trimmed.contains("content:")
}

fn collapse_plan_execution_summary_markdown(detail_markdown: &str) -> String {
    let source_lines = detail_markdown
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if source_lines.is_empty() {
        return String::new();
    }

    const MAX_DETAIL_LINES: usize = 4;
    let mut out = Vec::new();
    out.push(truncate_chars(source_lines[0], 160));

    let details = source_lines.iter().skip(1).collect::<Vec<_>>();
    for line in details.iter().take(MAX_DETAIL_LINES) {
        out.push(truncate_chars(line, 160));
    }
    if details.len() > MAX_DETAIL_LINES {
        out.push(format!("...(省略 {} 行)", details.len() - MAX_DETAIL_LINES));
    }
    out.join("\n")
}

fn parse_plan_execution_summary_markdown(content: &str) -> Option<String> {
    let mut lines = content.lines();
    let header = lines.next()?.trim();
    if header != "Plan 执行总结" {
        return None;
    }
    Some(lines.collect::<Vec<_>>().join("\n"))
}

fn format_tool_event_title(tool_name: &str, body: &str) -> String {
    if tool_name == "run_command"
        && let Some(command) = body
            .lines()
            .find_map(|line| line.trim().strip_prefix("命令: "))
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        return format!("Bash({})", truncate_chars(command, 64));
    }
    format!("工具执行 · {tool_name}")
}

fn parse_llm_stage(first_line: &str) -> Option<String> {
    if !first_line.starts_with("LLM 输出 [") {
        return None;
    }
    let start = first_line.find('[')? + 1;
    let rest = &first_line[start..];
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

fn extract_prefixed_line_value(content: &str, prefix: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(prefix).map(str::trim))
        .map(ToString::to_string)
}

fn extract_llm_content_preview(content: &str) -> Option<String> {
    let mut in_content = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if in_content {
            if trimmed.is_empty() {
                continue;
            }
            return Some(truncate_chars(trimmed, 160));
        }
        if trimmed == "content:" {
            in_content = true;
        }
    }
    None
}

fn format_stage_title(stage: &str) -> String {
    if stage == "planning-agent" {
        return "Planning".to_string();
    }
    if let Some(stripped) = stage.strip_prefix("execution-agent::") {
        let mut parts = stripped.splitn(2, "::");
        let plan = parts.next().unwrap_or("执行");
        let step = parts.next().unwrap_or("步骤");
        return format!("执行 · {plan} · {step}");
    }
    format!("LLM · {stage}")
}

fn build_event_title_line(title: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled("● ".to_string(), style),
        Span::styled(title.to_string(), style),
    ])
}

fn build_event_detail_line(detail: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled("  └ ".to_string(), style),
        Span::styled(detail.to_string(), style),
    ])
}

fn build_thinking_block_line(content: &str, style: Style, content_width: u16) -> Line<'static> {
    let padded = pad_line_to_width(content, content_width);
    Line {
        spans: vec![Span::styled(padded, style)],
        style,
        alignment: None,
    }
}

fn append_thinking_markdown_block(
    target: &mut Vec<Line<'static>>,
    markdown: &str,
    style: Style,
    content_width: u16,
) {
    let inner_width = content_width.saturating_sub(3).max(1);
    for line in markdown_to_owned_lines(markdown) {
        for wrapped in wrap_styled_line_to_width(line, inner_width) {
            target.push(build_thinking_markdown_line(wrapped, style, content_width));
        }
    }
}

fn build_thinking_markdown_line(
    line: Line<'static>,
    style: Style,
    content_width: u16,
) -> Line<'static> {
    let is_blank_markdown_line = line.spans.iter().all(|span| {
        let txt = span.content.as_ref();
        txt.trim().is_empty()
    });
    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    spans.push(Span::styled("   ".to_string(), style));

    let mut width = 3u16;
    for span in line.spans {
        let content = span.content.into_owned();
        width = width.saturating_add(text_display_width(&content));
        spans.push(Span::styled(content, style.patch(span.style)));
    }

    if width < content_width {
        let remaining = (content_width - width) as usize;
        if is_blank_markdown_line && remaining > 0 {
            if remaining > 1 {
                spans.push(Span::styled(" ".repeat(remaining - 1), style));
            }
            // 空白行末尾放一个 NBSP，避免整行被当作可裁剪空白导致背景断裂。
            spans.push(Span::styled("\u{00A0}".to_string(), style));
        } else {
            spans.push(Span::styled(" ".repeat(remaining), style));
        }
    } else if is_blank_markdown_line {
        // 极窄终端下无法补齐时，至少保证行内存在非普通空格字符，避免黑缝。
        if let Some(prefix) = spans.first_mut() {
            prefix.content = "  \u{00A0}".into();
        }
    }

    Line {
        spans,
        style,
        alignment: line.alignment,
    }
}

fn wrap_styled_line_to_width(line: Line<'static>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line];
    }

    if line.spans.is_empty() {
        return vec![Line {
            spans: vec![Span::raw(String::new())],
            style: line.style,
            alignment: line.alignment,
        }];
    }

    let mut wrapped_lines = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0u16;

    for span in line.spans {
        let span_style = span.style;
        for ch in span.content.chars() {
            if ch == '\r' {
                continue;
            }
            let ch_width = char_display_width(ch);
            if current_width > 0 && current_width.saturating_add(ch_width) > width {
                wrapped_lines.push(Line {
                    spans: std::mem::take(&mut current_spans),
                    style: line.style,
                    alignment: line.alignment,
                });
                current_width = 0;
            }

            push_char_to_styled_spans(&mut current_spans, ch, span_style);
            current_width = current_width.saturating_add(ch_width);

            if current_width >= width {
                wrapped_lines.push(Line {
                    spans: std::mem::take(&mut current_spans),
                    style: line.style,
                    alignment: line.alignment,
                });
                current_width = 0;
            }
        }
    }

    if !current_spans.is_empty() {
        wrapped_lines.push(Line {
            spans: current_spans,
            style: line.style,
            alignment: line.alignment,
        });
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(Line {
            spans: vec![Span::raw(String::new())],
            style: line.style,
            alignment: line.alignment,
        });
    }

    wrapped_lines
}

fn push_char_to_styled_spans(target: &mut Vec<Span<'static>>, ch: char, style: Style) {
    if let Some(last) = target.last_mut()
        && last.style == style
    {
        last.content.to_mut().push(ch);
        return;
    }
    target.push(Span::styled(ch.to_string(), style));
}

fn append_markdown_lines_with_prefix(
    target: &mut Vec<Line<'static>>,
    markdown: &str,
    prefix: &str,
) {
    for line in markdown_to_owned_lines(markdown) {
        target.push(prefix_line(line, prefix));
    }
}

fn append_styled_markdown_lines_with_prefix(
    target: &mut Vec<Line<'static>>,
    markdown: &str,
    prefix: &str,
    style: Style,
) {
    for line in markdown.split('\n') {
        let content = format!("{prefix}{line}");
        target.push(Line::from(Span::styled(content, style)));
    }
}

fn markdown_to_owned_lines(markdown: &str) -> Vec<Line<'static>> {
    if markdown.is_empty() {
        return vec![Line::default()];
    }

    let mut lines = Vec::new();
    let mut in_code_block = false;
    for raw_line in markdown.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let trimmed = line.trim_start();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        if in_code_block {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default()
                    .fg(Color::Rgb(202, 217, 202))
                    .add_modifier(Modifier::DIM),
            )));
            continue;
        }

        lines.push(parse_markdown_line(line));
    }

    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

fn parse_markdown_line(line: &str) -> Line<'static> {
    let trimmed = line.trim_start();
    let indent_len = line.len().saturating_sub(trimmed.len());
    let indent = " ".repeat(indent_len);

    if let Some((level, content)) = parse_heading(trimmed) {
        let mut spans = Vec::new();
        if !indent.is_empty() {
            spans.push(Span::raw(indent));
        }
        spans.extend(parse_inline_markdown(content, heading_style(level)));
        return Line::from(spans);
    }

    if let Some(content) = trimmed.strip_prefix("> ") {
        let mut spans = Vec::new();
        if !indent.is_empty() {
            spans.push(Span::raw(indent));
        }
        spans.push(Span::styled(
            "│ ".to_string(),
            Style::default().fg(Color::Gray),
        ));
        spans.extend(parse_inline_markdown(
            content,
            Style::default().fg(Color::Gray),
        ));
        return Line::from(spans);
    }

    if let Some(content) = parse_unordered_list_item(trimmed) {
        let mut spans = Vec::new();
        if !indent.is_empty() {
            spans.push(Span::raw(indent));
        }
        spans.push(Span::styled(
            "• ".to_string(),
            Style::default().fg(Color::Cyan),
        ));
        spans.extend(parse_inline_markdown(content, Style::default()));
        return Line::from(spans);
    }

    if let Some((index, content)) = parse_ordered_list_item(trimmed) {
        let mut spans = Vec::new();
        if !indent.is_empty() {
            spans.push(Span::raw(indent));
        }
        spans.push(Span::styled(
            format!("{index}. "),
            Style::default().fg(Color::Cyan),
        ));
        spans.extend(parse_inline_markdown(content, Style::default()));
        return Line::from(spans);
    }

    let mut spans = Vec::new();
    if !indent.is_empty() {
        spans.push(Span::raw(indent));
    }
    spans.extend(parse_inline_markdown(trimmed, Style::default()));
    Line::from(spans)
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let mut level = 0usize;
    for ch in line.chars() {
        if ch == '#' {
            level = level.saturating_add(1);
        } else {
            break;
        }
    }
    if (1..=6).contains(&level) && line.chars().nth(level) == Some(' ') {
        return Some((level, &line[level + 1..]));
    }
    None
}

fn parse_unordered_list_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
}

fn parse_ordered_list_item(line: &str) -> Option<(usize, &str)> {
    let mut digit_end = 0usize;
    for (idx, ch) in line.char_indices() {
        if ch.is_ascii_digit() {
            digit_end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if digit_end == 0 {
        return None;
    }
    let suffix = &line[digit_end..];
    if !suffix.starts_with(". ") {
        return None;
    }
    let number = line[..digit_end].parse::<usize>().ok()?;
    Some((number, &suffix[2..]))
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn parse_inline_markdown(line: &str, base_style: Style) -> Vec<Span<'static>> {
    if line.is_empty() {
        return vec![Span::raw(String::new())];
    }

    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut code = false;

    let chars: Vec<char> = line.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];

        if ch == '`' {
            flush_inline_buffer(
                &mut spans,
                &mut buffer,
                inline_style(base_style, bold, italic, strike, code),
            );
            code = !code;
            idx += 1;
            continue;
        }

        if !code && ch == '~' && idx + 1 < chars.len() && chars[idx + 1] == '~' {
            flush_inline_buffer(
                &mut spans,
                &mut buffer,
                inline_style(base_style, bold, italic, strike, code),
            );
            strike = !strike;
            idx += 2;
            continue;
        }

        if !code && ch == '*' && idx + 1 < chars.len() && chars[idx + 1] == '*' {
            flush_inline_buffer(
                &mut spans,
                &mut buffer,
                inline_style(base_style, bold, italic, strike, code),
            );
            bold = !bold;
            idx += 2;
            continue;
        }

        if !code && ch == '*' {
            flush_inline_buffer(
                &mut spans,
                &mut buffer,
                inline_style(base_style, bold, italic, strike, code),
            );
            italic = !italic;
            idx += 1;
            continue;
        }

        buffer.push(ch);
        idx += 1;
    }

    flush_inline_buffer(
        &mut spans,
        &mut buffer,
        inline_style(base_style, bold, italic, strike, code),
    );

    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

fn flush_inline_buffer(spans: &mut Vec<Span<'static>>, buffer: &mut String, style: Style) {
    if buffer.is_empty() {
        return;
    }
    let text = std::mem::take(buffer);
    spans.push(Span::styled(text, style));
}

fn heading_style(level: usize) -> Style {
    let color = match level {
        1 => Color::Yellow,
        2 => Color::LightYellow,
        3 => Color::LightCyan,
        _ => Color::White,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn inline_style(base: Style, bold: bool, italic: bool, strike: bool, code: bool) -> Style {
    let mut style = base;
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if strike {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if code {
        style = style
            .fg(Color::Rgb(255, 232, 168))
            .add_modifier(Modifier::UNDERLINED);
    }
    style
}

fn prefix_line(line: Line<'static>, prefix: &str) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(prefix.to_string()));
    spans.extend(line.spans);
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

pub(super) fn text_display_width(text: &str) -> u16 {
    const TAB_STOP: usize = 4;
    let mut col = 0usize;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let to_next_tab = TAB_STOP - (col % TAB_STOP);
                col = col.saturating_add(to_next_tab);
            }
            '\r' | '\n' => {}
            _ => {
                let w = UnicodeWidthChar::width(ch).unwrap_or(0);
                col = col.saturating_add(w);
            }
        }
    }
    col.min(u16::MAX as usize) as u16
}

fn char_display_width(ch: char) -> u16 {
    match ch {
        '\t' => 4,
        '\r' | '\n' => 0,
        _ => UnicodeWidthChar::width(ch).unwrap_or(0) as u16,
    }
}

fn pad_line_to_width(content: &str, width: u16) -> String {
    let mut padded = content.to_string();
    let current = text_display_width(content);
    if current < width {
        padded.push_str(&" ".repeat((width - current) as usize));
    }
    padded
}
