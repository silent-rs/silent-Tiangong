use std::sync::Arc;
use std::time::Duration;

use tiangong_core::terminal_trait::{TerminalExecResult, TerminalProvider};
use tiangong_core::tool::ToolResult;
use tokio::sync::mpsc;

use crate::types::TerminalCommand;
use crate::util::shell_quote;

const OUTPUT_THRESHOLD: usize = 2000;
const TERMINAL_INPUT_DEFAULT_WAIT_MS: u64 = 300;
const TERMINAL_INPUT_DEFAULT_OUTPUT_LINES: usize = 80;
const TERMINAL_INPUT_MAX_WAIT_MS: u64 = 5_000;

/// 将原始 cmd + args 格式化为终端可执行的命令字符串
fn format_command(cmd: &str, args: &[String]) -> String {
    if matches!(cmd, "bash" | "sh" | "powershell" | "pwsh") {
        // shell 命令：args 已经是 [flag, script] 格式，取 script 部分
        args.last().map(|s| s.as_str()).unwrap_or("").to_string()
    } else {
        // 普通命令：cmd + args 拼成一行
        let mut parts = vec![cmd.to_string()];
        for arg in args {
            parts.push(shell_quote(arg));
        }
        parts.join(" ")
    }
}

fn first_shell_word(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    trimmed
        .split(|c: char| c.is_whitespace() || matches!(c, '|' | '&' | ';' | '<' | '>'))
        .find(|word| !word.is_empty())
}

fn script_uses_interactive_terminal_program(script: &str) -> bool {
    script.lines().any(|line| {
        // Check first word of line (direct invocation: `vi file.txt`, `expect <<'EOF'`)
        if let Some(word) = first_shell_word(line) {
            if is_interactive_program(word) {
                return true;
            }
        }
        // Check for program names after pipe/semicolon (e.g., `printf ... | vi ...`)
        for segment in line.split(['|', ';']) {
            let word = segment.split_whitespace().next().unwrap_or("");
            if is_interactive_program(word) {
                return true;
            }
            // Check for `spawn <program>` pattern (inside expect heredocs)
            if let Some(rest) = segment.strip_prefix("spawn ") {
                let prog = rest.split_whitespace().next().unwrap_or("");
                if is_interactive_program(prog) {
                    return true;
                }
            }
        }
        false
    })
}

fn is_interactive_program(word: &str) -> bool {
    matches!(
        word.rsplit('/').next().unwrap_or(word),
        "vi" | "vim"
            | "nvim"
            | "nano"
            | "ssh"
            | "sftp"
            | "ftp"
            | "python"
            | "python3"
            | "node"
            | "irb"
            | "psql"
            | "mysql"
            | "sqlite3"
            | "expect"
    )
}

fn script_uses_stdin_automation(script: &str) -> bool {
    let lower = script.to_ascii_lowercase();
    script.contains("<<")
        || script.contains('|')
        || lower.contains("printf ")
        || lower.contains("echo ")
        || lower.contains(" -es")
        || lower.contains(" -e ")
        || lower.contains(" --cmd")
}

/// 将 LLM 输入字符串中字面形式的转义序列解码为真实控制字节。
///
/// JSON 在解析阶段已处理 `\n` `\r` `\t` `\\` `\uHHHH` 等标准转义；
/// 但 LLM 常出现以下"双转义"或非标准写法，会被原样写入 PTY，从而在 vi 等
/// 交互程序的插入模式下变成可见文本（如 `` 显示为 6 个字符）。
///
/// 这里再额外识别以下字面序列：
/// - `\uHHHH`（6 字符）→ 对应 Unicode 标量
/// - `\xHH`（4 字符）→ 对应字节
/// - `\e` 或 `\E`（2 字符）→ 0x1b（Esc）
/// - `\n` `\r` `\t` `\0` `\\`（2 字符）→ 对应控制字符（容错）
/// - `^X`（2 字符，X 为 A-Z/a-z/@/_/?）→ Ctrl+X
///
/// 此外，所有 LF（`\n`，0x0a）在写入 PTY 前会被替换为 CR（`\r`，0x0d）：
/// 现实终端里"回车键"实际发出的是 CR，PTY 线路规程再用 ICRNL 把 CR 转为 LF
/// 喂给应用。zsh 的 ZLE、vi/vim、less 等大量 TUI 程序在 raw 模式下只识别 CR
/// 作为"提交本行"，对 LF 无反应——这是历史会话中 `vi hello.txt\n` 后命令停在
/// 输入行不执行、`vvi hello.txti` 后追加字符无回显的根因。
///
/// 解码失败的字面反斜杠序列保持原样，避免影响普通文本输入。
fn decode_terminal_input(raw: &str) -> String {
    // 第一阶段：解码字面转义序列
    let decoded = if !raw.contains('\\') && !raw.contains('^') {
        raw.to_string()
    } else {
        decode_escape_sequences(raw)
    };

    // 第二阶段：把所有 LF 转为 CR，让 PTY 把"回车"正确识别为提交。
    // 已存在的 CR 保持不变，避免出现 CR+CR。
    let mut out = String::with_capacity(decoded.len());
    for ch in decoded.chars() {
        if ch == '\n' {
            out.push('\r');
        } else {
            out.push(ch);
        }
    }
    out
}

fn decode_escape_sequences(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            match next {
                b'u' if i + 5 < bytes.len() || (i + 5 == bytes.len()) => {
                    let hex = &raw[i + 2..i + 6.min(raw.len())];
                    if hex.len() == 4 && hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                        if let Ok(code) = u32::from_str_radix(hex, 16) {
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                                i += 6;
                                continue;
                            }
                        }
                    }
                    out.push('\\');
                    i += 1;
                    continue;
                }
                b'x' if i + 3 < bytes.len() => {
                    let hex = &raw[i + 2..i + 4];
                    if hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                        if let Ok(byte) = u8::from_str_radix(hex, 16) {
                            out.push(byte as char);
                            i += 4;
                            continue;
                        }
                    }
                    out.push('\\');
                    i += 1;
                    continue;
                }
                b'e' | b'E' => {
                    out.push('\u{001b}');
                    i += 2;
                    continue;
                }
                b'n' => {
                    out.push('\n');
                    i += 2;
                    continue;
                }
                b'r' => {
                    out.push('\r');
                    i += 2;
                    continue;
                }
                b't' => {
                    out.push('\t');
                    i += 2;
                    continue;
                }
                b'0' => {
                    out.push('\0');
                    i += 2;
                    continue;
                }
                b'\\' => {
                    out.push('\\');
                    i += 2;
                    continue;
                }
                _ => {}
            }
            out.push('\\');
            i += 1;
            continue;
        }
        if b == b'^' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            // ^@ = 0, ^A..^Z = 1..26, ^[ = Esc, ^\\ = 28, ^] = 29, ^^ = 30, ^_ = 31
            let mapped = match next {
                b'@' => Some(0u8),
                b'a'..=b'z' => Some(next - b'a' + 1),
                b'A'..=b'Z' => Some(next - b'A' + 1),
                b'[' => Some(0x1b),
                b'\\' => Some(0x1c),
                b']' => Some(0x1d),
                b'^' => Some(0x1e),
                b'_' => Some(0x1f),
                b'?' => Some(0x7f),
                _ => None,
            };
            if let Some(byte) = mapped {
                out.push(byte as char);
                i += 2;
                continue;
            }
        }
        // 默认：保留 UTF-8 字符（避免把多字节字符拆开）
        let ch_len = utf8_char_len(b);
        let end = (i + ch_len).min(bytes.len());
        if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
            out.push_str(s);
        } else {
            out.push(b as char);
        }
        i = end;
    }
    out
}

fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte >> 5 == 0b110 {
        2
    } else if first_byte >> 4 == 0b1110 {
        3
    } else if first_byte >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

fn reject_interactive_script(script: &str, interactive: bool) -> Option<ToolResult> {
    if interactive || !script_uses_interactive_terminal_program(script) {
        return None;
    }
    if !script_uses_stdin_automation(script) {
        return None;
    }
    Some(ToolResult {
        ok: false,
        summary: "交互式终端程序需要分步骤执行".to_string(),
        stdout: String::new(),
        stderr: "检测到脚本尝试用 heredoc、管道或 ex 模式自动驱动 vi/vim/nano/ssh 等交互式程序。请改为先调用 run_shell(interactive=true) 启动交互程序，再连续调用 terminal_input 发送按键，并根据每次返回的终端内容决定下一步；完成后再用 run_shell 执行验证命令。不要使用 vi -es、printf | vi 或 heredoc 驱动交互程序。".to_string(),
        exit_code: 2,
        execution: None,
    })
}

/// 通过 TerminalCommand channel 实现 TerminalProvider trait，支持面板打开时路由到交互 PTY
/// 终端能力实现：单 PTY 模型，所有执行/输入/输出/重置都路由到系统 PTY。
/// 历史上的双 PTY（系统 + 面板）已合并，不再有面板专属 PTY。
pub struct TerminalProviderImpl {
    system_tx: mpsc::Sender<TerminalCommand>,
}

impl TerminalProviderImpl {
    pub fn new(system_tx: mpsc::Sender<TerminalCommand>) -> Self {
        Self { system_tx }
    }
}

macro_rules! send_and_wait {
    ($tx:expr, $cmd:expr, $rx:expr, $timeout:expr) => {{
        if $tx.send($cmd).await.is_err() {
            return None;
        }
        tokio::time::timeout(Duration::from_secs($timeout), $rx)
            .await
            .ok()?
            .ok()?
    }};
}

impl TerminalProvider for TerminalProviderImpl {
    fn exec(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<TerminalExecResult>> + Send>>
    {
        let tx = self.system_tx.clone();
        let command = command.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let resp: crate::types::TerminalExecResponse = send_and_wait!(
                tx,
                TerminalCommand::Exec {
                    command,
                    timeout_secs,
                    response_tx
                },
                response_rx,
                180
            );
            Some(resp.into())
        })
    }

    fn exec_command(
        &self,
        cmd: &str,
        args: &[String],
        timeout_secs: Option<u64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<TerminalExecResult>> + Send>>
    {
        let command = format_command(cmd, args);
        self.exec(&command, timeout_secs)
    }

    fn exec_interactive(
        &self,
        command: &str,
        wait_secs: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<TerminalExecResult>> + Send>>
    {
        let tx = self.system_tx.clone();
        let command = command.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let resp: crate::types::TerminalExecResponse = send_and_wait!(
                tx,
                TerminalCommand::ExecInteractive {
                    command,
                    wait_secs,
                    response_tx
                },
                response_rx,
                180
            );
            Some(resp.into())
        })
    }

    fn exec_command_interactive(
        &self,
        cmd: &str,
        args: &[String],
        wait_secs: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<TerminalExecResult>> + Send>>
    {
        let command = format_command(cmd, args);
        self.exec_interactive(&command, wait_secs)
    }

    fn recent_output(
        &self,
        lines: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        let tx = self.system_tx.clone();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let output: String = send_and_wait!(
                tx,
                TerminalCommand::RecentOutput { lines, response_tx },
                response_rx,
                5
            );
            Some(output)
        })
    }

    fn current_cwd(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
        let tx = self.system_tx.clone();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let cwd: Option<String> = send_and_wait!(
                tx,
                TerminalCommand::CurrentCwd { response_tx },
                response_rx,
                5
            );
            cwd
        })
    }

    fn send_input(
        &self,
        input: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<()>> + Send>> {
        let tx = self.system_tx.clone();
        let input = input.to_string();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            send_and_wait!(
                tx,
                TerminalCommand::SendInput {
                    input,
                    source: crate::collaboration::InputSource::Agent,
                    response_tx,
                },
                response_rx,
                5
            );
            Some(())
        })
    }

    fn reset(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<()>> + Send>> {
        let tx = self.system_tx.clone();
        Box::pin(async move {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            send_and_wait!(tx, TerminalCommand::Reset { response_tx }, response_rx, 10);
            Some(())
        })
    }
}

/// 终端工具覆盖处理器（run_shell / terminal_output / terminal_input / terminal_reset）
/// 注意：run_command 不在此拦截，由 core 的 LocalToolExecutor 校验后自动路由到 PTY
pub struct TerminalToolOverride {
    provider: Arc<dyn TerminalProvider>,
}

impl TerminalToolOverride {
    pub fn new(provider: Arc<dyn TerminalProvider>) -> Self {
        Self { provider }
    }

    fn truncate_text(text: &str, max_chars: usize) -> String {
        let mut chars = text.chars();
        let mut value: String = chars.by_ref().take(max_chars).collect();
        if chars.next().is_some() {
            value.push_str("...\n[输出已截断，可使用 terminal_output 工具查看完整输出]");
        }
        value
    }

    fn handle_run_shell(
        provider: &Arc<dyn TerminalProvider>,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let command = match call.arguments.get("script").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return Box::pin(async { None }),
        };
        let timeout_secs = call.arguments.get("timeout").and_then(|v| v.as_u64());
        let interactive = call
            .arguments
            .get("interactive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // 读取 agent 指定的工作目录（run_shell schema 含 cwd 字段）。
        // 与 run_command_via_pty 一致：cwd 与终端当前目录不同时，把 script
        // 包装成 `cd <cwd> && <script>`，避免命令在错误的目录执行。
        let cwd = call
            .arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(String::from);

        if let Some(result) = reject_interactive_script(&command, interactive) {
            return Box::pin(async move { Some(result) });
        }

        let provider = provider.clone();
        Box::pin(async move {
            // 若 agent 指定了 cwd 且与终端当前 cwd 不同，包装为 cd <cwd> && <script>。
            // exec_command trait 方法不携带 cwd，这里手动前置 cd。
            let command = match cwd.as_deref() {
                Some(want) if !want.is_empty() => {
                    let current = provider.current_cwd().await;
                    let need_cd = match current.as_deref() {
                        Some(cur) => !cur
                            .trim_end_matches('/')
                            .eq_ignore_ascii_case(want.trim_end_matches('/')),
                        None => true,
                    };
                    if need_cd {
                        format!("cd {} && {}", shell_quote(want), command)
                    } else {
                        command
                    }
                }
                _ => command,
            };
            let result = if interactive {
                match provider.exec_interactive(&command, 3).await {
                    Some(r) => r,
                    None => return None,
                }
            } else {
                match provider.exec(&command, timeout_secs).await {
                    Some(r) => r,
                    None => return None, // 终端不可用，回退到默认 run_shell
                }
            };

            let stdout = Self::truncate_text(&result.stdout, OUTPUT_THRESHOLD);

            let mut summary = if result.interactive_mode {
                "命令已进入交互模式".to_string()
            } else if result.timed_out {
                "命令执行超时".to_string()
            } else if result.interrupted_by_user {
                "命令被用户中断".to_string()
            } else if result.exit_code != 0 {
                format!("命令执行失败（退出码 {}）", result.exit_code)
            } else {
                "命令执行成功".to_string()
            };

            if !result.cwd_after.is_empty() {
                summary.push_str(&format!("（cwd: {}）", result.cwd_after));
            }

            let mut stderr = result.stderr.clone();
            if result.interactive_mode {
                stderr.push_str(
                    "\n[提示] 命令已进入交互模式，可使用 terminal_input 发送键盘输入（如 \\x03 发送 Ctrl+C），terminal_output 查看终端输出",
                );
            } else if result.interrupted_by_user {
                stderr.push_str("\n[提示] 命令被用户中断，建议询问用户是否需要调整执行计划");
            }

            Some(ToolResult {
                ok: result.exit_code == 0 && !result.timed_out,
                summary,
                stdout,
                stderr,
                exit_code: result.exit_code,
                execution: None,
            })
        })
    }

    fn handle_terminal_output(
        provider: &Arc<dyn TerminalProvider>,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let lines = call
            .arguments
            .get("lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;

        let provider = provider.clone();
        Box::pin(async move {
            let output = match provider.recent_output(lines).await {
                Some(o) => o,
                None => {
                    return Some(ToolResult {
                        ok: false,
                        summary: "终端会话不可用".to_string(),
                        stdout: String::new(),
                        stderr: "终端未初始化或已关闭".to_string(),
                        exit_code: 1,
                        execution: None,
                    });
                }
            };

            Some(ToolResult {
                ok: true,
                summary: format!("最近 {} 行终端输出", lines),
                stdout: output,
                stderr: String::new(),
                exit_code: 0,
                execution: None,
            })
        })
    }

    fn handle_terminal_input(
        provider: &Arc<dyn TerminalProvider>,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let raw_input = match call.arguments.get("input").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => {
                return Box::pin(async {
                    Some(ToolResult {
                        ok: false,
                        summary: "缺少 input 参数".to_string(),
                        stdout: String::new(),
                        stderr: "必须提供 input 参数".to_string(),
                        exit_code: 1,
                        execution: None,
                    })
                });
            }
        };
        let input = decode_terminal_input(&raw_input);
        let wait_ms = call
            .arguments
            .get("wait_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(TERMINAL_INPUT_DEFAULT_WAIT_MS)
            .min(TERMINAL_INPUT_MAX_WAIT_MS);
        let lines = call
            .arguments
            .get("lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(TERMINAL_INPUT_DEFAULT_OUTPUT_LINES as u64)
            .clamp(1, 500) as usize;

        let provider = provider.clone();
        Box::pin(async move {
            match provider.send_input(&input).await {
                Some(_) => {
                    if wait_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                    }
                    let stdout = provider
                        .recent_output(lines)
                        .await
                        .map(|o| Self::truncate_text(&o, OUTPUT_THRESHOLD))
                        .unwrap_or_default();
                    Some(ToolResult {
                        ok: true,
                        summary: format!("已发送输入到终端，返回最近 {} 行终端显示内容", lines),
                        stdout,
                        stderr: "[提示] 如果终端仍在等待输入或没有结束标记，请继续使用 terminal_input；需要更多上下文时使用 terminal_output。".to_string(),
                        exit_code: 0,
                        execution: None,
                    })
                }
                None => Some(ToolResult {
                    ok: false,
                    summary: "终端会话不可用".to_string(),
                    stdout: String::new(),
                    stderr: "终端未初始化或已关闭".to_string(),
                    exit_code: 1,
                    execution: None,
                }),
            }
        })
    }

    fn handle_terminal_reset(
        provider: &Arc<dyn TerminalProvider>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let provider = provider.clone();
        Box::pin(async move {
            match provider.reset().await {
                Some(_) => Some(ToolResult {
                    ok: true,
                    summary: "终端会话已重置".to_string(),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                }),
                None => Some(ToolResult {
                    ok: false,
                    summary: "终端会话不可用".to_string(),
                    stdout: String::new(),
                    stderr: "终端未初始化或已关闭".to_string(),
                    exit_code: 1,
                    execution: None,
                }),
            }
        })
    }
}

impl tiangong_core::tool_override::ToolOverrideHandler for TerminalToolOverride {
    fn handle(
        &self,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        match call.name.as_str() {
            "run_shell" => Self::handle_run_shell(&self.provider, call),
            "terminal_output" => Self::handle_terminal_output(&self.provider, call),
            "terminal_input" => Self::handle_terminal_input(&self.provider, call),
            "terminal_reset" => Self::handle_terminal_reset(&self.provider),
            _ => Box::pin(async { None }),
        }
    }
}

/// 终端工具规格提供者：注册 terminal_input / terminal_output / terminal_reset 工具
pub struct TerminalToolSpecProvider;

impl tiangong_core::tool_override::ToolSpecProvider for TerminalToolSpecProvider {
    fn tool_specs(&self) -> Vec<tiangong_core::model::ToolSpec> {
        use tiangong_core::model::ToolSpec;
        vec![
            ToolSpec {
                name: "terminal_input".to_string(),
                description: "向终端发送键盘输入，用于与交互式程序进行分步操作。发送后会短暂等待并直接返回当前终端显示内容；如果终端仍在等待输入或没有结束标记，应继续调用 terminal_input。".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "input": { "type": "string", "description": "要发送的键盘输入文本。支持控制字符：\\u0003=Ctrl+C 中断, \\u001b=Esc, \\u0004=Ctrl+D EOF, \\u0015=Ctrl+U 清行, \\n=回车。示例：i 进入插入模式, \\u001b 退出插入模式, :wq\\n 保存退出vi, exit()\\n 退出Python REPL" },
                        "wait_ms": { "type": "integer", "description": "发送后等待终端响应的毫秒数，默认 300，最大 5000", "minimum": 0, "maximum": 5000 },
                        "lines": { "type": "integer", "description": "返回最近 N 行终端显示内容，默认 80", "minimum": 1, "maximum": 500 }
                    },
                    "required": ["input"]
                }),
            },
            ToolSpec {
                name: "terminal_output".to_string(),
                description: "获取终端最近的输出内容。用于查看交互式程序的当前屏幕状态，或在 terminal_input 发送输入后查看程序响应。".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "lines": { "type": "integer", "description": "返回最近 N 行输出，默认 50", "minimum": 1, "maximum": 500 }
                    },
                    "required": []
                }),
            },
            ToolSpec {
                name: "terminal_reset".to_string(),
                description: "重置终端会话（破坏性操作）：重启 shell 进程并清空输出缓冲区，会丢弃当前终端中所有未保存的状态（包括正在运行的 vi/nano 等交互程序）。仅在 PTY 完全卡死、显示乱码无法恢复、命令无响应且 terminal_input 已尝试发送 Ctrl+C (\\u0003) 与 Esc (\\u001b) 仍无效时使用。一般的命令超时、交互对话框（如 vi swap 文件提示）等情况不要调用本工具。".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        ]
    }
}

/// 终端 Prompt 规则提供者：注入交互模式相关规则
pub struct TerminalPromptSectionProvider;

impl tiangong_core::tool_override::PromptSectionProvider for TerminalPromptSectionProvider {
    fn prompt_sections(&self) -> Vec<String> {
        vec![
            "当用户请求涉及命令行操作（安装依赖、创建项目、编译构建、文件操作等），必须逐步使用 `run_shell` 实际执行命令，不要仅以文本或代码块形式展示命令给用户。每步执行一条命令，根据执行结果决定下一步操作。回复中可以用代码块展示已执行的命令，但必须先通过 `run_shell` 实际执行。".to_string(),
            "涉及交互式程序（vi/vim/nano/python REPL/ssh/安装向导等）或多步终端操作时，先在回复开头用简短列表列出关键步骤（例如：1. 启动 vi；2. 进入插入模式；3. 输入文本；4. 保存退出；5. 验证结果），再依次执行工具调用。每个工具调用只完成一个原子动作，依据返回结果决定下一个工具调用。".to_string(),
            "需要使用交互式程序时，必须使用 `run_shell(interactive=true)` 启动程序，然后通过 `terminal_input` 逐步发送键盘输入。`terminal_input` 每次都会返回当前终端显示内容；没有结束标记但终端在等待输入时，应根据返回内容继续调用 `terminal_input`，直到程序完成或需要中断。".to_string(),
            "用户明确要求使用特定交互式程序（如「用 vi 写」「用 nano 编辑」）时，必须通过 `run_shell(interactive=true)` + `terminal_input` 在终端中操作该程序完成，不能用 `write_file`、`echo >`、`printf >` 等非交互方式绕过。即使记忆中建议使用替代方案，也必须遵守用户的明确要求。".to_string(),
            "禁止用 heredoc、管道、printf/echo 喂输入、`vi -es`、vim ex 模式、expect 或批处理模式来替代真实交互。不要尝试自动化驱动交互式程序，只能通过 `terminal_input` 逐步发送按键。".to_string(),
            "`terminal_input` 发送控制字符：`\\u0003`=Ctrl+C、`\\u001b`=Esc、`\\n`=回车（系统会自动把 `\\n` 转为真实键盘的 CR 信号）。每次发送后先阅读工具返回的终端内容，再决定下一步；需要更多上下文时使用 `terminal_output`。".to_string(),
            "已经具备 `run_shell`、`terminal_input`、`terminal_output`、`terminal_reset` 工具，可以驱动任何交互式终端程序。禁止在回复中声称「无法操作」「没有交互工具」「环境不支持」而放弃；遇到困难应先调用 `terminal_output` 查看当前状态，再决定下一步操作。".to_string(),
            "`terminal_input` 连续多次返回完全相同的输出（屏幕没有任何变化）通常意味着上一次输入没有被终端接收或回显——不要把它当成「终端卡死」。先尝试：1) 调用 `terminal_output(lines=50)` 拿到更完整的画面，确认当前实际所在模式（shell 提示符 vs vi 界面 vs 程序对话框）；2) 如果停在 shell 提示符但前一次 run_shell 的命令没执行，发送单个 `\\n` 触发提交，再 `terminal_output` 确认；3) 如果在 vi/vim 内但按键无反应，先发送 `\\u001b`（Esc）回到 Normal 模式再继续。上述任何一步只要屏幕发生变化就说明输入路径正常，继续推进即可。".to_string(),
            "`terminal_reset` 会强制重启 shell 并丢弃当前 PTY 内的全部状态——若 vi/nano/pico 等编辑器正在前台运行，会留下 `.文件名.swp` swap 文件。仅在以下情形才允许调用：(a) `terminal_input \\u0003` 与 `\\u001b` 均已尝试且 `terminal_output` 显示终端无任何变化；(b) 终端输出大量乱码、`stty` 状态被破坏、shell 完全无响应。命令超时、命令未找到、退出码非零、用户请求未完成等都不是调用 `terminal_reset` 的理由——这类情况应正常分析错误并继续推进。".to_string(),
            "终端交互失败时的恢复策略应优先级：1) 调用 `terminal_output` 查看当前终端状态；2) 通过 `terminal_input` 发送 `\\u0003`（Ctrl+C）中断当前前台程序；3) 通过 `terminal_input` 发送 `\\u001b`（Esc）退出对话框/模式；4) 若是 vi/vim 因 swap 文件进入恢复选择界面，发送 `q\\n` 退出后用 `run_shell` 删除 `.文件名.swp`/`.文件名.swx` 再重新启动；5) 仅当上述都无效且终端彻底卡死时才使用 `terminal_reset`。命令执行超时或返回非零退出码绝不构成调用 `terminal_reset` 的理由。".to_string(),
            "vi/vim 启动若出现 swap 恢复对话框（如 `Found a swap file by the name ...`、`Open Read-Only/Edit anyway/Recover/Quit` 等），按以下顺序处理：发送 `q\\n` 退出 vi → 用 `run_shell(interactive=false)` 执行 `rm -f .目标文件.swp .目标文件.swx` → 重新 `run_shell(interactive=true, vi 目标文件)`。绝不在只读模式下继续编辑，也不要因此调用 `terminal_reset` 或切换到 `write_file` 等非交互方式。".to_string(),
            "`recall_memory` 返回的「替代方案」「不可交互」类建议仅作参考；当用户已明确指定使用 vi/vim/nano/ssh 等交互式程序时，必须坚持在终端中完成，不要因为记忆建议切换到 `write_file`、`echo >` 等方式。记忆中关于「环境不支持交互」「工具不可用」的描述已经过时，当前环境已具备完整的终端交互能力。".to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_heredoc_driven_vi_without_interactive_mode() {
        let script = "rm -f hello.txt\nvi -es hello.txt <<'EOF'\ni\nworld\n.\nwq\nEOF\n";
        let result = reject_interactive_script(script, false);
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(!result.ok);
        assert_eq!(result.exit_code, 2);
        assert!(result.stderr.contains("terminal_input"));
    }

    #[test]
    fn rejects_expect_driven_vi() {
        let script =
            "expect <<'EOF'\nset timeout 10\nspawn vi hello.txt\nsend \"iworld\\033:wq\\r\"\nexpect eof\nEOF";
        let result = reject_interactive_script(script, false);
        assert!(result.is_some());
        assert!(result.unwrap().stderr.contains("terminal_input"));
    }

    #[test]
    fn rejects_pipe_driven_vi() {
        let script = "printf 'iworld\\033:wq\\n' | vi hello.txt";
        let result = reject_interactive_script(script, false);
        assert!(result.is_some());
        assert!(result.unwrap().stderr.contains("terminal_input"));
    }

    #[test]
    fn allows_interactive_vi_launch() {
        let result = reject_interactive_script("vi hello.txt", true);
        assert!(result.is_none());
    }

    #[test]
    fn allows_regular_shell_script() {
        let result = reject_interactive_script("rm -f hello.txt\ncat hello.txt", false);
        assert!(result.is_none());
    }

    #[test]
    fn allows_vi_without_automation() {
        // Simple vi invocation without heredoc/pipe should not be rejected
        let result = reject_interactive_script("vi hello.txt", false);
        assert!(result.is_none());
    }

    #[test]
    fn decode_literal_unicode_escape_to_ctrl_c() {
        // LLM 常把 `` 当字面 6 字符发出，应解码为 0x03
        let decoded = decode_terminal_input("\\u0003");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded.as_bytes()[0], 0x03);
    }

    #[test]
    fn decode_literal_unicode_escape_to_esc() {
        let decoded = decode_terminal_input("\\u001b");
        assert_eq!(decoded.as_bytes(), &[0x1b]);
    }

    #[test]
    fn decode_literal_hex_escape() {
        let decoded = decode_terminal_input("\\x1b");
        assert_eq!(decoded.as_bytes(), &[0x1b]);
    }

    #[test]
    fn decode_short_esc_alias() {
        let decoded = decode_terminal_input("\\e");
        assert_eq!(decoded.as_bytes(), &[0x1b]);
        let decoded_upper = decode_terminal_input("\\E");
        assert_eq!(decoded_upper.as_bytes(), &[0x1b]);
    }

    #[test]
    fn decode_ctrl_caret_notation() {
        assert_eq!(decode_terminal_input("^C").as_bytes(), &[0x03]);
        assert_eq!(decode_terminal_input("^[").as_bytes(), &[0x1b]);
        assert_eq!(decode_terminal_input("^M").as_bytes(), &[0x0d]);
        assert_eq!(decode_terminal_input("^?").as_bytes(), &[0x7f]);
    }

    #[test]
    fn decode_mixed_escape_and_text() {
        // 模拟 vi 保存退出：Esc + ":wq\n"
        // 注：LF 在写入 PTY 前会被转为 CR（zsh ZLE / vim 等只识别 CR 为提交）
        let decoded = decode_terminal_input("\\u001b:wq\\n");
        let bytes = decoded.as_bytes();
        assert_eq!(bytes[0], 0x1b);
        assert_eq!(&bytes[1..4], b":wq");
        assert_eq!(bytes[4], b'\r');
    }

    #[test]
    fn decode_preserves_plain_text_and_utf8() {
        let decoded = decode_terminal_input("hello 世界");
        assert_eq!(decoded, "hello 世界");
    }

    #[test]
    fn decode_leaves_unknown_escape_unchanged() {
        // 未知转义不应被吞掉
        let decoded = decode_terminal_input("\\q");
        assert_eq!(decoded, "\\q");
    }

    #[test]
    fn decode_translates_lf_to_cr_for_tty() {
        // PTY 输入路径下，LF 会被映射为 CR（真实键盘 Enter 键发出的是 CR）
        let decoded = decode_terminal_input("i\n");
        assert_eq!(decoded.as_bytes(), b"i\r");
    }
}
