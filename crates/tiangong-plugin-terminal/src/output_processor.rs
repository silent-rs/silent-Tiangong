use std::sync::{Arc, Mutex};

use tauri::Emitter;
use tracing::{info, warn};

use crate::manager::{push_output, TerminalState};
use crate::types::{contains_marker, TerminalOutputEvent};

const READ_BUF_SIZE: usize = 4096;

/// 终端行处理器，模拟光标行为以正确处理 zsh 行编辑器的重绘。
/// 维护 pending 缓冲区处理跨 chunk 的不完整 ESC 序列。
pub(crate) struct TerminalLineProcessor {
    line: Vec<char>,
    cursor: usize,
    pending: String,
}

impl TerminalLineProcessor {
    pub fn new() -> Self {
        Self {
            line: Vec::new(),
            cursor: 0,
            pending: String::new(),
        }
    }

    pub fn process(&mut self, raw: &str) -> Vec<String> {
        if !self.pending.is_empty() {
            self.pending.push_str(raw);
        }
        let input = if self.pending.is_empty() {
            raw.to_string()
        } else {
            std::mem::take(&mut self.pending)
        };

        let mut complete_lines = Vec::new();
        let chars: Vec<char> = input.chars().collect();
        let len = chars.len();
        let mut i = 0;

        while i < len {
            let c = chars[i];
            match c {
                '\n' => {
                    let line: String = self.line.iter().collect();
                    if !line.trim().is_empty() {
                        complete_lines.push(line);
                    }
                    self.line.clear();
                    self.cursor = 0;
                    i += 1;
                }
                '\r' => {
                    self.cursor = 0;
                    i += 1;
                }
                '\x1b' => {
                    let seq_start = i;
                    i += 1;
                    if i >= len {
                        self.pending = chars[seq_start..].iter().collect();
                        break;
                    }
                    match chars[i] {
                        '[' => {
                            i += 1;
                            let mut params = String::new();
                            let mut found_final = false;
                            while i < len {
                                let next = chars[i];
                                if next.is_ascii()
                                    && ((next as u8).is_ascii_digit() || next == ';' || next == '?')
                                {
                                    params.push(next);
                                    i += 1;
                                } else if next.is_ascii() && (b'@'..=b'~').contains(&(next as u8)) {
                                    self.handle_csi(&params, next);
                                    i += 1;
                                    found_final = true;
                                    break;
                                } else {
                                    break;
                                }
                            }
                            if !found_final {
                                self.pending = chars[seq_start..].iter().collect();
                                break;
                            }
                        }
                        ']' | 'P' => {
                            i += 1;
                            let mut found_end = false;
                            while i < len {
                                let next = chars[i];
                                if next == '\x07' {
                                    i += 1;
                                    found_end = true;
                                    break;
                                }
                                if next == '\x1b' && i + 1 < len && chars[i + 1] == '\\' {
                                    i += 2;
                                    found_end = true;
                                    break;
                                }
                                i += 1;
                            }
                            if !found_end {
                                self.pending = chars[seq_start..].iter().collect();
                                break;
                            }
                        }
                        '(' | ')' => {
                            i += 1;
                            if i >= len {
                                self.pending = chars[seq_start..].iter().collect();
                                break;
                            }
                            i += 1;
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }
                c if c.is_control() => {
                    i += 1;
                }
                c => {
                    if self.cursor >= self.line.len() {
                        self.line.push(c);
                    } else {
                        self.line[self.cursor] = c;
                    }
                    self.cursor += 1;
                    i += 1;
                }
            }
        }

        complete_lines
    }

    pub fn current_line(&self) -> String {
        self.line.iter().collect()
    }

    fn handle_csi(&mut self, params: &str, final_byte: char) {
        match final_byte {
            'K' => {
                let n: usize = params.trim_start_matches('?').parse().unwrap_or(0);
                match n {
                    0 => self.line.truncate(self.cursor),
                    1 => {
                        let after: Vec<char> = self.line.drain(self.cursor..).collect();
                        self.line = after;
                        self.cursor = 0;
                    }
                    2 => {
                        self.line.clear();
                        self.cursor = 0;
                    }
                    _ => {}
                }
            }
            'G' => {
                let col: usize = params.parse().unwrap_or(1).max(1);
                self.cursor = col - 1;
                while self.line.len() < self.cursor {
                    self.line.push(' ');
                }
            }
            'J' => {
                let n: usize = params.parse().unwrap_or(0);
                if n >= 2 {
                    self.line.clear();
                    self.cursor = 0;
                }
            }
            'C' => {
                let n: usize = params.parse().unwrap_or(1);
                for _ in 0..n {
                    if self.cursor < self.line.len() {
                        self.cursor += 1;
                    } else {
                        self.line.push(' ');
                        self.cursor = self.line.len();
                    }
                }
            }
            'D' => {
                let n: usize = params.parse().unwrap_or(1);
                self.cursor = self.cursor.saturating_sub(n);
            }
            'P' => {
                let n: usize = params.parse().unwrap_or(1);
                for _ in 0..n {
                    if self.cursor < self.line.len() {
                        self.line.remove(self.cursor);
                    }
                }
            }
            '@' => {
                let n: usize = params.parse().unwrap_or(1);
                for _ in 0..n {
                    self.line.insert(self.cursor, ' ');
                }
            }
            _ => {}
        }
    }
}

/// 行级 marker 过滤器：过滤包含内部 marker 的完整行，正常输出实时透传。
/// 使用小窗口暂存策略处理 marker 跨 chunk 分割。
pub(crate) struct RawOutputFilter {
    pending: String,
}

/// marker 公共前缀
const MARKER_PREFIX: &str = "__TIANGONG_";
/// pending 缓冲区上限
const MAX_PENDING: usize = 8192;

impl RawOutputFilter {
    pub fn new() -> Self {
        Self {
            pending: String::new(),
        }
    }

    /// 处理一个原始 chunk，返回过滤掉 marker 行后的文本（用于推送 xterm.js）
    pub fn filter(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut result = String::new();

        // 1. 处理所有完整行（以 \n 结尾）
        while let Some(pos) = self.pending.find('\n') {
            let line = self.pending[..=pos].to_string();
            self.pending = self.pending[pos + 1..].to_string();
            if !contains_marker(&line) {
                result.push_str(&line);
            }
        }

        // 2. 处理剩余不完整文本
        if !self.pending.is_empty() {
            if contains_marker(&self.pending) || self.pending.contains(MARKER_PREFIX) {
                // 包含 marker 或公共前缀 → 暂存等换行（可能是 marker 行）
                // 超限时直接输出（marker 行不可能这么长）
                if self.pending.len() > MAX_PENDING {
                    result.push_str(&self.pending);
                    self.pending.clear();
                }
            } else {
                // 不含 marker → 检查尾部是否可能是 marker 前缀的开头
                let split = self.safe_split_point();
                if split > 0 {
                    result.push_str(&self.pending[..split]);
                    self.pending = self.pending[split..].to_string();
                }
            }
        }

        result
    }

    /// 计算可以安全输出的切分点（字节偏移，保证 UTF-8 字符边界安全）。
    /// 尾部保留可能是 `__TIANGONG_` 前缀的片段，前部输出。
    fn safe_split_point(&self) -> usize {
        for prefix_len in (1..MARKER_PREFIX.len()).rev() {
            if self.pending.len() >= prefix_len {
                let split = self.pending.len() - prefix_len;
                // 确保切分点在 UTF-8 字符边界上
                if self.pending.is_char_boundary(split) {
                    let tail = &self.pending[split..];
                    if MARKER_PREFIX.starts_with(tail) {
                        return split;
                    }
                }
            }
        }
        self.pending.len()
    }
}

/// 后台读取 PTY 输出并推送到环形缓冲区和前端
pub(crate) fn spawn_output_reader(
    reader: Arc<Mutex<Box<dyn std::io::Read + Send>>>,
    state: Arc<Mutex<TerminalState>>,
    app: tauri::AppHandle,
    session_id: String,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; READ_BUF_SIZE];
        let mut processor = TerminalLineProcessor::new();
        let mut output_filter = RawOutputFilter::new();

        loop {
            let n = {
                let mut reader = match reader.lock() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::TimedOut {
                            warn!(error = %e, "PTY 读取错误");
                        }
                        continue;
                    }
                }
            };

            let raw_text = String::from_utf8_lossy(&buf[..n]).to_string();

            // 行级 marker 过滤后推送给 xterm.js
            let filtered = output_filter.filter(&raw_text);
            if !filtered.is_empty() {
                let event = TerminalOutputEvent {
                    session_id: session_id.clone(),
                    text: filtered,
                    is_echo: false,
                };
                if let Err(e) = app.emit("terminal:output", &event) {
                    warn!(error = %e, "推送终端输出事件失败");
                }
            }

            // 行处理器用于内部缓冲区（exec 命令需要检测 marker，所以 marker 行必须写入 buffer）
            let complete_lines = processor.process(&raw_text);

            {
                let mut state = match state.lock() {
                    Ok(s) => s,
                    Err(_) => break,
                };
                state.current_line = processor.current_line();
                for line in &complete_lines {
                    push_output(&mut state, line.clone());
                }
            }
        }

        info!(session_id = %session_id, "PTY 输出读取线程退出");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_processor_basic() {
        let mut p = TerminalLineProcessor::new();
        let lines = p.process("hello\nworld\n");
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn test_line_processor_ansi_colors() {
        let mut p = TerminalLineProcessor::new();
        let lines = p.process("\x1b[32mhello\x1b[0m world\n");
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn test_line_processor_osc() {
        let mut p = TerminalLineProcessor::new();
        let lines = p.process("\x1b]0;title\x07content\n");
        assert_eq!(lines, vec!["content"]);
    }

    #[test]
    fn test_line_processor_cr_overwrite() {
        let mut p = TerminalLineProcessor::new();
        // zsh 重绘: 先写 abc，然后 \r 回行首，ESC[K 清行，再写 def
        let lines = p.process("abc\r\x1b[Kdef\n");
        assert_eq!(lines, vec!["def"]);
    }

    #[test]
    fn test_line_processor_zsh_redraw() {
        let mut p = TerminalLineProcessor::new();
        // 模拟 zsh 输入 "ls" 时的多次重绘
        let raw = "\x1b[0G\x1b[K> l\x1b[0G\x1b[K> ls\r\n";
        let lines = p.process(raw);
        assert_eq!(lines, vec!["> ls"]);
    }

    #[test]
    fn test_line_processor_cursor_position() {
        let mut p = TerminalLineProcessor::new();
        let lines = p.process("\x1b[10Ghello\n");
        assert_eq!(lines, vec!["         hello"]);
    }

    // ===== RawOutputFilter tests =====

    #[test]
    fn test_filter_marker_line_removed() {
        let mut f = RawOutputFilter::new();
        let out = f.filter("hello\n__TIANGONG_START_abc123__\nworld\n");
        assert_eq!(out, "hello\nworld\n");
    }

    #[test]
    fn test_filter_normal_output_passes_through() {
        let mut f = RawOutputFilter::new();
        let out = f.filter("prompt> ");
        assert_eq!(out, "prompt> ");
    }

    #[test]
    fn test_filter_prompt_without_newline() {
        let mut f = RawOutputFilter::new();
        let out = f.filter("$ ");
        assert_eq!(out, "$ ");
    }

    #[test]
    fn test_filter_marker_cross_chunk_filtered() {
        let mut f = RawOutputFilter::new();
        // marker 被拆成两个 chunk
        let out1 = f.filter("__TIANGONG_STA");
        assert_eq!(out1, ""); // 尾部 `__T` 是 marker 前缀，暂存
        let out2 = f.filter("RT_xxx__\n");
        assert_eq!(out2, ""); // 完整行含 marker，被过滤
    }

    #[test]
    fn test_filter_mixed_output() {
        let mut f = RawOutputFilter::new();
        let out = f.filter("hello\n__TIANGONG_START_x__\nworld\n");
        assert_eq!(out, "hello\nworld\n");
    }

    #[test]
    fn test_filter_progress_update_passes() {
        let mut f = RawOutputFilter::new();
        let out = f.filter("\rProgress: 50%");
        assert_eq!(out, "\rProgress: 50%");
    }

    #[test]
    fn test_filter_marker_then_normal() {
        let mut f = RawOutputFilter::new();
        // marker 行被过滤，后续正常输出透传
        let out1 = f.filter("__TIANGONG_START_x__\n");
        assert_eq!(out1, "");
        let out2 = f.filter("result line\n");
        assert_eq!(out2, "result line\n");
    }

    #[test]
    fn test_filter_marker_split_after_one_underscore() {
        let mut f = RawOutputFilter::new();
        // marker 在 `_` 后被拆分，`_` 是 marker 前缀的一部分
        let out1 = f.filter("result_");
        assert_eq!(out1, "result"); // `result` 输出，`_` 暂存
        let out2 = f.filter("_TIANGONG_START_x__\n");
        assert_eq!(out2, ""); // 完整行含 marker，被过滤
    }

    #[test]
    fn test_filter_marker_split_after_two_underscores() {
        let mut f = RawOutputFilter::new();
        // marker 在 `__` 后被拆分
        let out1 = f.filter("output__");
        assert_eq!(out1, "output"); // `output` 输出，`__` 暂存
        let out2 = f.filter("TIANGONG_START_x__\n");
        assert_eq!(out2, ""); // 完整行含 marker，被过滤
    }

    #[test]
    fn test_filter_utf8_before_marker_prefix_suffix() {
        let mut f = RawOutputFilter::new();
        // UTF-8 字符 `中` 后跟 `__`，需要正确处理字符边界
        let out1 = f.filter("中__");
        assert_eq!(out1, "中"); // `中` 输出，`__` 暂存
        let out2 = f.filter("TIANGONG_START_x__\n");
        assert_eq!(out2, ""); // 完整行含 marker，被过滤
    }
}
