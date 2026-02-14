use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use unicode_width::UnicodeWidthChar;

use crate::core::session::{Message, MessageRole};

pub(super) fn format_transcript(messages: &[Message], content_width: u16) -> Text<'static> {
    if messages.is_empty() {
        return Text::raw("开始新对话吧。");
    }

    let mut lines = Vec::new();
    for message in messages {
        let role = match message.role {
            MessageRole::User => "你",
            MessageRole::Assistant => "天工",
            MessageRole::System => "系统",
        };
        let content = message.content.trim_end();
        let display_content = if content.trim().is_empty() {
            "..."
        } else {
            content
        };
        let user_style = Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(47, 63, 86))
            .add_modifier(Modifier::BOLD);
        let thinking_style = Style::default()
            .fg(Color::Rgb(196, 214, 235))
            .bg(Color::Rgb(35, 47, 64))
            .add_modifier(Modifier::ITALIC);

        if message.role == MessageRole::User {
            let mut parts = display_content.lines();
            let first = parts.next().unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!(" {role}: {first} "),
                user_style,
            )));
            for part in parts {
                lines.push(Line::from(Span::styled(format!("   {part} "), user_style)));
            }
        } else {
            if message.role == MessageRole::Assistant
                && !message.reasoning_content.trim().is_empty()
            {
                lines.push(Line::from(Span::raw(format!("{role}:"))));
                lines.push(build_thinking_block_line(
                    "   [思考]",
                    thinking_style,
                    content_width,
                ));
                append_thinking_markdown_block(
                    &mut lines,
                    message.reasoning_content.trim_end(),
                    thinking_style,
                    content_width,
                );
                lines.push(build_thinking_block_line(
                    "   [/思考]",
                    thinking_style,
                    content_width,
                ));
                if !content.trim().is_empty() {
                    append_markdown_lines_with_prefix(&mut lines, content, "   ");
                }
                lines.push(Line::default());
                continue;
            }
            append_markdown_lines_with_role(&mut lines, role, display_content);
        }
        lines.push(Line::default());
    }
    Text::from(lines)
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

fn append_markdown_lines_with_role(target: &mut Vec<Line<'static>>, role: &str, markdown: &str) {
    let mut lines = markdown_to_owned_lines(markdown);
    if lines.is_empty() {
        target.push(Line::from(Span::raw(format!("{role}:"))));
        return;
    }

    let first = lines.remove(0);
    target.push(prefix_line(first, &format!("{role}: ")));
    for line in lines {
        target.push(prefix_line(line, "   "));
    }
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
                    .bg(Color::Rgb(36, 45, 42)),
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

fn parse_inline_markdown(line: &str, base_style: Style) -> Vec<Span<'static>> {
    if line.is_empty() {
        return vec![Span::raw(String::new())];
    }

    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut code = false;

    let chars: Vec<char> = line.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];

        if ch == '`' {
            flush_inline_buffer(
                &mut spans,
                &mut buffer,
                inline_style(base_style, bold, italic, code),
            );
            code = !code;
            idx += 1;
            continue;
        }

        if !code && ch == '*' && idx + 1 < chars.len() && chars[idx + 1] == '*' {
            flush_inline_buffer(
                &mut spans,
                &mut buffer,
                inline_style(base_style, bold, italic, code),
            );
            bold = !bold;
            idx += 2;
            continue;
        }

        if !code && ch == '*' {
            flush_inline_buffer(
                &mut spans,
                &mut buffer,
                inline_style(base_style, bold, italic, code),
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
        inline_style(base_style, bold, italic, code),
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

fn inline_style(base: Style, bold: bool, italic: bool, code: bool) -> Style {
    let mut style = base;
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if code {
        style = style
            .fg(Color::Rgb(255, 232, 168))
            .bg(Color::Rgb(44, 44, 46));
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
