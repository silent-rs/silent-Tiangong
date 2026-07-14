use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tauri::Emitter;
use tracing::{info, warn};

use crate::manager::{push_output, TerminalState};
use crate::types::{contains_marker, TerminalOutputEvent};

const READ_BUF_SIZE: usize = 4096;

/// 终端输出持久化日志器：把系统 PTY 的 marker 过滤后输出追加写到磁盘，
/// 应用重启后可回填到环形缓冲区，实现「终端历史保留」。
///
/// 仅用于系统 PTY（跨会话全局）；面板交互 PTY 不落盘。
pub(crate) struct OutputLogger {
    file: Arc<Mutex<File>>,
    path: PathBuf,
}

/// 日志文件大小上限（1 MiB），超过则保留尾部一半后重写，防无限增长
const MAX_LOG_BYTES: u64 = 1024 * 1024;

impl OutputLogger {
    /// 打开（或创建）日志文件。失败时返回 None，调用方应优雅降级（不持久化）。
    pub fn open(path: PathBuf) -> Option<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .map_err(|e| warn!(error = %e, path = %path.display(), "打开终端日志文件失败"))
            .ok()?;
        Some(Self {
            file: Arc::new(Mutex::new(file)),
            path,
        })
    }

    /// 追加写一段文本。超过上限时滚动（保留尾部一半），防日志无限膨胀。
    /// 写失败仅记录警告，不影响终端主流程。
    pub fn append(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut guard = match self.file.lock() {
            Ok(f) => f,
            Err(_) => return,
        };
        if let Err(e) = guard.write_all(text.as_bytes()) {
            warn!(error = %e, "写终端日志失败");
            return;
        }
        // 滚动检查：追加后若超过上限，保留尾部一半重写
        if let Ok(meta) = guard.metadata() {
            let len = meta.len();
            if len > MAX_LOG_BYTES {
                if let Err(e) = rotate_tail(&mut guard, len) {
                    warn!(error = %e, "滚动终端日志失败");
                }
            }
        }
    }

    /// 清空日志（用户主动重置终端时调用）。
    pub fn clear(&self) {
        let mut guard = match self.file.lock() {
            Ok(f) => f,
            Err(_) => return,
        };
        if let Err(e) = guard
            .set_len(0)
            .and_then(|_| guard.seek(SeekFrom::Start(0)))
        {
            warn!(error = %e, "清空终端日志失败");
        }
    }

    /// 日志文件路径（供回填/调试用）
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 滚动日志：读取文件尾部一半，truncate 后重写，使文件回到约一半大小。
///
/// 注意：`len / 2` 是任意字节偏移，可能落在 UTF-8 多字节字符中间。
/// 直接 `read_to_string` 会因无效 UTF-8 返回 `InvalidData`，导致滚动失败、
/// 日志无限增长。因此先跳过到下一个 `\n`（丢弃可能不完整的行），再读取。
fn rotate_tail(file: &mut File, len: u64) -> std::io::Result<()> {
    let keep_from = len / 2;
    file.seek(SeekFrom::Start(keep_from))?;
    // 跳过 keep_from 处可能不完整的首行（避免 UTF-8 字符被截断）
    let mut skip_buf = [0u8; 1];
    while file.read(&mut skip_buf)? > 0 {
        if skip_buf[0] == b'\n' {
            break;
        }
    }
    let mut tail = String::new();
    file.read_to_string(&mut tail)?;
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(tail.as_bytes())?;
    Ok(())
}

/// 读取日志文件末尾最多 `max_lines` 行，用于启动时回填环形缓冲区。
/// 返回 (按行分割的 Vec<String>, 完整文本)。失败返回空。
pub(crate) fn read_log_tail(path: &Path, max_lines: usize) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut lines: Vec<&str> = content.lines().collect();
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    lines.into_iter().map(String::from).collect()
}

/// 把一行历史输出回填进环形缓冲区（启动时从日志恢复历史用）。
/// 复用 manager 的 push_output，保证缓冲区计数与正常写入一致。
pub(crate) fn backfill_line(state: &mut crate::manager::TerminalState, line: String) {
    crate::manager::push_output(state, line);
}

/// 终端行处理器，模拟光标行为以正确处理 zsh 行编辑器的重绘。
///
/// 序列分帧委托给 [`vte::Parser`]（Paul Williams 状态机实现），保证所有合法的
/// CSI/OSC/ESC 序列都能被正确分帧——包括真彩色冒号参数（`ESC[38:2::r:g:bm`）、
/// DEC private 模式（`ESC[?25l`）等。光标行为模拟在 [`LineBuildHandler`] 的
/// `csi_dispatch` / `execute` 回调中实现。
pub(crate) struct TerminalLineProcessor {
    parser: vte::Parser,
    handler: LineBuildHandler,
}

impl TerminalLineProcessor {
    pub fn new() -> Self {
        Self {
            parser: vte::Parser::new(),
            handler: LineBuildHandler::new(),
        }
    }

    pub fn process(&mut self, raw: &str) -> Vec<String> {
        // vte::Parser 内部维护状态机，跨 chunk 的不完整序列自动暂存在 parser 里，
        // 无需手写 pending 缓冲区。
        self.parser.advance(&mut self.handler, raw.as_bytes());
        std::mem::take(&mut self.handler.complete_lines)
    }

    pub fn current_line(&self) -> String {
        self.handler.line.iter().collect()
    }
}

/// [`vte::Perform`] 实现：维护单行字符缓冲 + 光标位置，收集完整行。
///
/// 只模拟影响单行内容的光标行为（K/G/J/C/D/P/@），SGR（颜色）等序列在
/// `csi_dispatch` 的 `_ => {}` 分支被忽略——颜色不影响行内容收集。
struct LineBuildHandler {
    line: Vec<char>,
    cursor: usize,
    complete_lines: Vec<String>,
}

impl LineBuildHandler {
    fn new() -> Self {
        Self {
            line: Vec::new(),
            cursor: 0,
            complete_lines: Vec::new(),
        }
    }

    /// 从 vte Params 提取第一个参数值（默认 0）。
    fn first_param(params: &vte::Params) -> usize {
        params.iter().next().map(|p| p[0]).unwrap_or(0) as usize
    }

    fn handle_csi(&mut self, params: &vte::Params, final_byte: char) {
        // DEC private 序列（如 ESC[?25l 隐藏光标）的参数带 ? 前缀，
        // vte 已剥离前缀，直接取数值。
        match final_byte {
            // ESC[K — 清行
            'K' => {
                let n = Self::first_param(params);
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
            // ESC[G — 光标水平绝对定位
            'G' => {
                let col = Self::first_param(params).max(1);
                self.cursor = col - 1;
                while self.line.len() < self.cursor {
                    self.line.push(' ');
                }
            }
            // ESC[J — 清屏（单行模型只处理 2=全清）
            'J' => {
                let n = Self::first_param(params);
                if n >= 2 {
                    self.line.clear();
                    self.cursor = 0;
                }
            }
            // ESC[C — 光标右移
            'C' => {
                let n = Self::first_param(params).max(1);
                for _ in 0..n {
                    if self.cursor < self.line.len() {
                        self.cursor += 1;
                    } else {
                        self.line.push(' ');
                        self.cursor = self.line.len();
                    }
                }
            }
            // ESC[D — 光标左移
            'D' => {
                let n = Self::first_param(params).max(1);
                self.cursor = self.cursor.saturating_sub(n);
            }
            // ESC[P — 删除字符
            'P' => {
                let n = Self::first_param(params).max(1);
                for _ in 0..n {
                    if self.cursor < self.line.len() {
                        self.line.remove(self.cursor);
                    }
                }
            }
            // ESC[@ — 插入空白
            '@' => {
                let n = Self::first_param(params).max(1);
                for _ in 0..n {
                    self.line.insert(self.cursor, ' ');
                }
            }
            // SGR（颜色/样式）等其他序列不影响行内容收集，忽略
            _ => {}
        }
    }
}

impl vte::Perform for LineBuildHandler {
    fn print(&mut self, c: char) {
        if self.cursor >= self.line.len() {
            self.line.push(c);
        } else {
            self.line[self.cursor] = c;
        }
        self.cursor += 1;
    }

    fn execute(&mut self, byte: u8) {
        // C0 控制字符
        match byte {
            b'\n' => {
                // LF：提交当前行
                let line: String = self.line.iter().collect();
                if !line.trim().is_empty() {
                    self.complete_lines.push(line);
                }
                self.line.clear();
                self.cursor = 0;
            }
            b'\r' => {
                // CR：光标回行首
                self.cursor = 0;
            }
            // 其他控制字符（BS/HT 等）忽略
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        c: char,
    ) {
        self.handle_csi(params, c);
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

/// 后台读取 PTY 输出并推送到环形缓冲区和前端。
/// `logger` 为 Some 时同时把 marker 过滤后的输出落盘（仅系统 PTY 传 Some）。
pub(crate) fn spawn_output_reader(
    reader: Arc<Mutex<Box<dyn std::io::Read + Send>>>,
    state: Arc<Mutex<TerminalState>>,
    app: tauri::AppHandle,
    session_id: String,
    logger: Option<Arc<OutputLogger>>,
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
                // 落盘（marker 过滤后的纯文本，与 xterm 看到的一致）
                if let Some(ref logger) = logger {
                    logger.append(&filtered);
                }
                // session_id 从 state 动态读取：草稿态 PTY 转正时会更新 state.session_id，
                // 这样事件能以新（真实）session_id 推送，前端按 session_id 分发能正确命中。
                let current_session_id = state
                    .lock()
                    .map(|s| s.session_id.clone())
                    .unwrap_or_else(|_| session_id.clone());
                let event = TerminalOutputEvent {
                    session_id: current_session_id,
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

    #[test]
    fn test_line_processor_gh_spinner_plus_markers() {
        // 模拟 gh 在 PTY 下的完整输出：spinner 动画 + 彩色 JSON + marker 行。
        // 验证 TerminalLineProcessor 能正确 push marker 行（不被 spinner 的
        // CR/清行序列破坏行模拟），这是 #237 超时 bug 的核心时序场景。
        let mut p = TerminalLineProcessor::new();

        // gh 的 spinner：隐藏光标 + 多帧动画（CR + spinner字符 + CR + ESC[K）
        let spinner = "\x1b[?25l\r\x1b[K\r⣾\r\x1b[K\r⣽\r\x1b[K\r⣻\r\x1b[K\r\x1b[?25h\r\x1b[K";

        // gh 的彩色 JSON 输出（每行以 \r\n 结尾）
        let gh_json = "\x1b[1;37m{\x1b[m\r\n  \x1b[1;34m\"number\"\x1b[m\x1b[1;37m:\x1b[m 237\r\n\x1b[1;37m}\x1b[m\r\n";

        // wrapper 注入的 marker 行
        let markers =
            "__TIANGONG_CWD_abc__/tmp\r\n__TIANGONG_RC_abc__0\r\n__TIANGONG_END_abc__\r\n";

        // 场景1：分两个 chunk 喂入（模拟 PTY 读取线程的实际行为）
        let lines = p.process(&format!("{}{}", spinner, gh_json));
        // spinner 不产生完整行；JSON 产生 3 行（ANSI 已剥离）
        assert_eq!(lines, vec!["{", "  \"number\": 237", "}"]);

        let lines2 = p.process(markers);
        // marker 行必须被正确 push
        assert_eq!(
            lines2,
            vec![
                "__TIANGONG_CWD_abc__/tmp",
                "__TIANGONG_RC_abc__0",
                "__TIANGONG_END_abc__",
            ]
        );
    }

    #[test]
    fn test_line_processor_gh_single_mega_chunk() {
        // 场景2：spinner + JSON + markers 合并到一个超大 chunk（PTY 读取线程
        // 可能一次性读取 4096 字节，所有内容在一个 chunk 里）。
        let mut p = TerminalLineProcessor::new();
        let spinner = "\x1b[?25l\r\x1b[K\r⣾\r\x1b[K\r⣽\r\x1b[K\r⣻\r\x1b[K\r\x1b[?25h\r\x1b[K";
        let gh_json = "\x1b[1;37m{\x1b[m\r\n  \x1b[1;34m\"number\"\x1b[m\x1b[1;37m:\x1b[m 237\r\n\x1b[1;37m}\x1b[m\r\n";
        let markers =
            "__TIANGONG_CWD_abc__/tmp\r\n__TIANGONG_RC_abc__0\r\n__TIANGONG_END_abc__\r\n";

        let all = format!("{}{}{}", spinner, gh_json, markers);
        let lines = p.process(&all);

        // 必须包含 marker 行——如果行模拟被 spinner 破坏，marker 可能丢失
        let has_rc = lines.iter().any(|l| l.contains("__TIANGONG_RC_abc__"));
        let has_end = lines.iter().any(|l| l.contains("__TIANGONG_END_abc__"));
        assert!(has_rc, "RC marker 行丢失！lines: {:?}", lines);
        assert!(has_end, "END marker 行丢失！lines: {:?}", lines);
    }

    #[test]
    fn test_line_processor_truecolor_colon_params() {
        // 真彩色序列使用冒号分隔参数（ITU-T T.416）：ESC[38:2::255:0:0m
        // 旧版只接受数字+;+?，遇到冒号会卡住处理器，后续 marker 无法进入缓冲区。
        let mut p = TerminalLineProcessor::new();
        let input = "\x1b[38:2::255:0:0mred text\x1b[0m\n__TIANGONG_END_x__\n";
        let lines = p.process(input);
        // 冒号序列被正确消费，不卡死；red text 和 marker 行正常 push
        assert_eq!(lines, vec!["red text", "__TIANGONG_END_x__"]);
    }
}
