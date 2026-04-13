use std::io::{self, IsTerminal, Write};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal;

use crate::completion::{self, CompletionCandidate};

/// 交互式输入读取器
pub struct InputReader {
    history: Vec<String>,
    history_pos: Option<usize>,
    stashed_input: String,
}

impl InputReader {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
            history_pos: None,
            stashed_input: String::new(),
        }
    }

    pub fn push_history(&mut self, input: &str) {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|last| last == trimmed) {
            return;
        }
        self.history.push(trimmed.to_string());
        if self.history.len() > 200 {
            self.history.remove(0);
        }
    }

    pub fn read_line<F>(&mut self, prompt: &str, complete_fn: F) -> Result<Option<String>>
    where
        F: Fn(&str, usize) -> Vec<CompletionCandidate>,
    {
        if !io::stdin().is_terminal() {
            return self.read_line_plain(prompt);
        }
        self.read_line_interactive(prompt, &complete_fn)
    }

    fn read_line_plain(&mut self, prompt: &str) -> Result<Option<String>> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut line = String::new();
        let n = io::stdin().read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim().to_string()))
    }

    fn read_line_interactive<F>(&mut self, prompt: &str, complete_fn: &F) -> Result<Option<String>>
    where
        F: Fn(&str, usize) -> Vec<CompletionCandidate>,
    {
        print!("{prompt}");
        io::stdout().flush()?;

        terminal::enable_raw_mode()?;
        let result = self.raw_loop(prompt, complete_fn);
        terminal::disable_raw_mode()?;
        result
    }

    fn raw_loop<F>(&mut self, prompt: &str, complete_fn: &F) -> Result<Option<String>>
    where
        F: Fn(&str, usize) -> Vec<CompletionCandidate>,
    {
        let mut buf = String::new();
        let mut cursor: usize = 0;
        let mut hints: Vec<CompletionCandidate> = Vec::new();
        let mut hint_lines_shown: u16 = 0;
        let mut selected_hint: Option<usize> = None;
        let mut stdout = io::stdout();

        loop {
            let Event::Key(key) = event::read()? else {
                continue;
            };

            match key.code {
                // Tab / Shift+Tab：在已有候选中循环选择并应用
                KeyCode::Tab | KeyCode::BackTab => {
                    if hints.is_empty() {
                        continue;
                    }
                    let idx = match (key.code, selected_hint) {
                        (KeyCode::Tab, None) => 0,
                        (KeyCode::Tab, Some(i)) => (i + 1) % hints.len(),
                        (KeyCode::BackTab, None) => hints.len() - 1,
                        (KeyCode::BackTab, Some(0)) => hints.len() - 1,
                        (KeyCode::BackTab, Some(i)) => i - 1,
                        _ => 0,
                    };
                    selected_hint = Some(idx);
                    apply_completion(&mut buf, &mut cursor, &hints[idx].clone());
                    clear_hint_lines(&mut stdout, hint_lines_shown)?;
                    hint_lines_shown = show_hint_list(&mut stdout, &hints, selected_hint)?;
                    redraw(&mut stdout, prompt, &buf, cursor)?;
                    continue;
                }

                KeyCode::Enter => {
                    clear_hint_lines(&mut stdout, hint_lines_shown)?;
                    write!(stdout, "\r\n")?;
                    stdout.flush()?;
                    self.history_pos = None;
                    return Ok(Some(buf));
                }

                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    clear_hint_lines(&mut stdout, hint_lines_shown)?;
                    hint_lines_shown = 0;
                    hints.clear();
                    selected_hint = None;
                    if buf.is_empty() {
                        write!(stdout, "^C\r\n")?;
                        stdout.flush()?;
                        self.history_pos = None;
                        return Ok(None); // 空输入时退出
                    }
                    // 有内容时清空
                    buf.clear();
                    cursor = 0;
                    self.history_pos = None;
                    write!(stdout, "^C\r\n")?;
                    stdout.flush()?;
                    redraw(&mut stdout, prompt, &buf, cursor)?;
                    continue;
                }

                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if buf.is_empty() {
                        clear_hint_lines(&mut stdout, hint_lines_shown)?;
                        write!(stdout, "\r\n")?;
                        stdout.flush()?;
                        return Ok(None);
                    }
                }

                KeyCode::Esc => {
                    if !hints.is_empty() {
                        clear_hint_lines(&mut stdout, hint_lines_shown)?;
                        hints.clear();
                        hint_lines_shown = 0;
                        selected_hint = None;
                        continue;
                    }
                    if !buf.is_empty() {
                        buf.clear();
                        cursor = 0;
                        self.history_pos = None;
                        redraw(&mut stdout, prompt, &buf, cursor)?;
                    }
                    continue;
                }

                KeyCode::Backspace => {
                    if cursor > 0 {
                        let prev = char_to_byte(&buf, cursor - 1);
                        let curr = char_to_byte(&buf, cursor);
                        buf.replace_range(prev..curr, "");
                        cursor -= 1;
                    }
                }
                KeyCode::Delete => {
                    let len = buf.chars().count();
                    if cursor < len {
                        let curr = char_to_byte(&buf, cursor);
                        let next = char_to_byte(&buf, cursor + 1);
                        buf.replace_range(curr..next, "");
                    }
                }
                KeyCode::Left => {
                    cursor = cursor.saturating_sub(1);
                }
                KeyCode::Right => {
                    if cursor < buf.chars().count() {
                        cursor += 1;
                    }
                }
                KeyCode::Home => cursor = 0,
                KeyCode::End => cursor = buf.chars().count(),
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    cursor = 0;
                }
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    cursor = buf.chars().count();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let byte_pos = char_to_byte(&buf, cursor);
                    buf.replace_range(..byte_pos, "");
                    cursor = 0;
                }
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let byte_pos = char_to_byte(&buf, cursor);
                    buf.truncate(byte_pos);
                }

                KeyCode::Up => {
                    // 有候选列表时：上移选择
                    if !hints.is_empty() {
                        let idx = match selected_hint {
                            Some(0) | None => hints.len() - 1,
                            Some(i) => i - 1,
                        };
                        selected_hint = Some(idx);
                        apply_completion(&mut buf, &mut cursor, &hints[idx].clone());
                        clear_hint_lines(&mut stdout, hint_lines_shown)?;
                        hint_lines_shown = show_hint_list(&mut stdout, &hints, selected_hint)?;
                        redraw(&mut stdout, prompt, &buf, cursor)?;
                        continue;
                    }
                    // 无候选：历史导航
                    if self.history.is_empty() {
                        continue;
                    }
                    match self.history_pos {
                        None => {
                            self.stashed_input = buf.clone();
                            self.history_pos = Some(self.history.len() - 1);
                        }
                        Some(pos) if pos > 0 => {
                            self.history_pos = Some(pos - 1);
                        }
                        _ => continue,
                    }
                    buf.clone_from(&self.history[self.history_pos.unwrap()]);
                    cursor = buf.chars().count();
                }

                KeyCode::Down => {
                    // 有候选列表时：下移选择
                    if !hints.is_empty() {
                        let idx = match selected_hint {
                            None => 0,
                            Some(i) => (i + 1) % hints.len(),
                        };
                        selected_hint = Some(idx);
                        apply_completion(&mut buf, &mut cursor, &hints[idx].clone());
                        clear_hint_lines(&mut stdout, hint_lines_shown)?;
                        hint_lines_shown = show_hint_list(&mut stdout, &hints, selected_hint)?;
                        redraw(&mut stdout, prompt, &buf, cursor)?;
                        continue;
                    }
                    // 无候选：历史导航
                    match self.history_pos {
                        Some(pos) if pos + 1 < self.history.len() => {
                            self.history_pos = Some(pos + 1);
                            buf.clone_from(&self.history[pos + 1]);
                        }
                        Some(_) => {
                            self.history_pos = None;
                            buf.clone_from(&self.stashed_input);
                        }
                        None => continue,
                    }
                    cursor = buf.chars().count();
                }

                KeyCode::Char(ch) => {
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                        let byte_pos = char_to_byte(&buf, cursor);
                        buf.insert(byte_pos, ch);
                        cursor += 1;
                        self.history_pos = None;
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }

            // ---- 每次按键后：刷新显示 + 自动检测补全 ----
            clear_hint_lines(&mut stdout, hint_lines_shown)?;
            hint_lines_shown = 0;
            selected_hint = None;
            redraw(&mut stdout, prompt, &buf, cursor)?;

            // 自动检测补全触发
            let new_hints = complete_fn(&buf, cursor);
            if !new_hints.is_empty() {
                hints = new_hints;
                hint_lines_shown = show_hint_list(&mut stdout, &hints, None)?;
                redraw(&mut stdout, prompt, &buf, cursor)?;
            } else {
                hints.clear();
            }
        }
    }
}

fn apply_completion(buf: &mut String, cursor: &mut usize, candidate: &CompletionCandidate) {
    if let Some((trigger, start, _prefix)) = completion::detect_trigger(buf, *cursor) {
        let byte_start = char_to_byte(buf, start);
        let byte_cursor = char_to_byte(buf, *cursor);
        let mut replacement = candidate.value.clone();
        if trigger == completion::CompletionTrigger::AtMention {
            replacement.push(' ');
        }
        buf.replace_range(byte_start..byte_cursor, &replacement);
        *cursor = start + replacement.chars().count();
    }
}

fn redraw(stdout: &mut io::Stdout, prompt: &str, buf: &str, cursor: usize) -> Result<()> {
    // prompt 包含 ANSI 颜色码，计算可见宽度
    let prompt_visible_width = visible_len(prompt);
    let display_cursor = prompt_visible_width + display_width(buf, cursor);
    write!(stdout, "\r\x1b[2K{prompt}{buf}")?;
    write!(stdout, "\r\x1b[{}C", display_cursor)?;
    stdout.flush()?;
    Ok(())
}

/// 竖排显示候选列表，返回占用的行数
fn show_hint_list(
    stdout: &mut io::Stdout,
    candidates: &[CompletionCandidate],
    selected: Option<usize>,
) -> Result<u16> {
    let max_show = 10.min(candidates.len());
    if max_show == 0 {
        return Ok(0);
    }

    // 向下输出候选行
    for (i, c) in candidates.iter().take(max_show).enumerate() {
        write!(stdout, "\r\n\x1b[2K")?;
        let is_selected = selected == Some(i);
        if is_selected {
            write!(stdout, "  \x1b[7m {:<20} {}\x1b[0m", c.label, c.hint)?;
        } else {
            write!(stdout, "  \x1b[90m {:<20} {}\x1b[0m", c.label, c.hint)?;
        }
    }

    // 光标回到输入行
    write!(stdout, "\x1b[{}A", max_show)?;
    stdout.flush()?;

    Ok(max_show as u16)
}

/// 清除候选列表行
fn clear_hint_lines(stdout: &mut io::Stdout, lines: u16) -> Result<()> {
    if lines == 0 {
        return Ok(());
    }
    // 保存位置，逐行清除，再回来
    for _ in 0..lines {
        write!(stdout, "\r\n\x1b[2K")?;
    }
    write!(stdout, "\x1b[{}A", lines)?;
    stdout.flush()?;
    Ok(())
}

fn char_to_byte(s: &str, char_pos: usize) -> usize {
    s.char_indices()
        .nth(char_pos)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn display_width(s: &str, char_pos: usize) -> usize {
    let byte_pos = char_to_byte(s, char_pos);
    s[..byte_pos].chars().map(char_display_width).sum()
}

/// 计算去掉 ANSI 转义序列后的可见字符宽度
fn visible_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            len += char_display_width(c);
        }
    }
    len
}

/// 字符显示宽度（终端列数）
fn char_display_width(c: char) -> usize {
    if c.is_ascii() {
        return 1;
    }
    // CJK 统一表意文字 + 扩展区 → 宽度 2
    let cp = c as u32;
    if (0x4E00..=0x9FFF).contains(&cp)       // CJK 基本
        || (0x3400..=0x4DBF).contains(&cp)    // CJK 扩展 A
        || (0x20000..=0x2A6DF).contains(&cp)  // CJK 扩展 B
        || (0xF900..=0xFAFF).contains(&cp)    // CJK 兼容
        || (0x3000..=0x303F).contains(&cp)    // CJK 符号
        || (0xFF01..=0xFF60).contains(&cp)    // 全角形式
        || (0xFFE0..=0xFFE6).contains(&cp)    // 全角符号
        || (0x1100..=0x115F).contains(&cp)    // 韩文 Jamo
        || (0xAC00..=0xD7AF).contains(&cp)
    // 韩文音节
    {
        return 2;
    }
    // 其他 Unicode（Emoji 部分符号、拉丁扩展等）→ 宽度 1
    1
}
