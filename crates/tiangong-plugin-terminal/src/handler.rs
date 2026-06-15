use std::sync::Arc;
use std::time::Duration;

use tiangong_core::terminal_trait::{TerminalExecResult, TerminalProvider};
use tiangong_core::tool::ToolResult;
use tokio::sync::mpsc;

use crate::types::TerminalCommand;
use crate::util::shell_quote;

const OUTPUT_THRESHOLD: usize = 2000;

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

/// 拦截尝试自动驱动交互式终端程序的脚本。
///
/// 当前环境不支持交互式终端程序（terminal_input/output/reset 已移除）。
/// 检测到脚本尝试用 heredoc、管道、ex 模式等驱动 vi/vim/nano/ssh 等交互式程序时，
/// 拒绝并引导改用 write_file/sed 等非交互方式。
fn reject_interactive_script(script: &str) -> Option<ToolResult> {
    if !script_uses_interactive_terminal_program(script) {
        return None;
    }
    if !script_uses_stdin_automation(script) {
        return None;
    }
    Some(ToolResult {
        ok: false,
        summary: "当前环境不支持交互式终端程序".to_string(),
        stdout: String::new(),
        stderr: "检测到脚本尝试用 heredoc、管道或 ex 模式自动驱动 vi/vim/nano/ssh 等交互式程序。当前环境暂不支持交互式终端程序，请改用 write_file、sed、awk 等非交互方式完成文件编辑或自动化操作。".to_string(),
        exit_code: 2,
        execution: None,
    })
}

/// 终端能力实现：单 PTY 模型，所有执行都路由到系统 PTY。
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

/// 终端工具覆盖处理器（run_shell）。
/// 注意：run_command 不在此拦截，由 core 的 LocalToolExecutor 校验后自动路由到 PTY。
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
            value.push_str("...\n[输出已截断]");
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
        // 读取 agent 指定的工作目录（run_shell schema 含 cwd 字段）。
        // 与 run_command_via_pty 一致：cwd 与终端当前目录不同时，把 script
        // 包装成 `cd <cwd> && <script>`，避免命令在错误的目录执行。
        let cwd = call
            .arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(String::from);

        if let Some(result) = reject_interactive_script(&command) {
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
            let result = match provider.exec(&command, timeout_secs).await {
                Some(r) => r,
                None => return None, // 终端不可用，回退到默认 run_shell
            };

            let stdout = Self::truncate_text(&result.stdout, OUTPUT_THRESHOLD);

            let mut summary = if result.timed_out {
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
            if result.interrupted_by_user {
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
}

impl tiangong_core::tool_override::ToolOverrideHandler for TerminalToolOverride {
    fn handle(
        &self,
        call: &tiangong_core::model::ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        match call.name.as_str() {
            "run_shell" => Self::handle_run_shell(&self.provider, call),
            _ => Box::pin(async { None }),
        }
    }
}

/// 终端 Prompt 规则提供者：注入基础命令执行规则
pub struct TerminalPromptSectionProvider;

impl tiangong_core::tool_override::PromptSectionProvider for TerminalPromptSectionProvider {
    fn prompt_sections(&self) -> Vec<String> {
        vec![
            "当用户请求涉及命令行操作（安装依赖、创建项目、编译构建、文件操作等），必须逐步使用 `run_shell` 实际执行命令，不要仅以文本或代码块形式展示命令给用户。每步执行一条命令，根据执行结果决定下一步操作。回复中可以用代码块展示已执行的命令，但必须先通过 `run_shell` 实际执行。".to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_heredoc_driven_vi() {
        let script = "rm -f hello.txt\nvi -es hello.txt <<'EOF'\ni\nworld\n.\nwq\nEOF\n";
        let result = reject_interactive_script(script);
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(!result.ok);
        assert_eq!(result.exit_code, 2);
    }

    #[test]
    fn rejects_expect_driven_vi() {
        let script =
            "expect <<'EOF'\nset timeout 10\nspawn vi hello.txt\nsend \"iworld\\033:wq\\r\"\nexpect eof\nEOF";
        let result = reject_interactive_script(script);
        assert!(result.is_some());
    }

    #[test]
    fn rejects_pipe_driven_vi() {
        let script = "printf 'iworld\\033:wq\\n' | vi hello.txt";
        let result = reject_interactive_script(script);
        assert!(result.is_some());
    }

    #[test]
    fn allows_regular_shell_script() {
        let result = reject_interactive_script("rm -f hello.txt\ncat hello.txt");
        assert!(result.is_none());
    }

    #[test]
    fn allows_vi_without_automation() {
        // Simple vi invocation without heredoc/pipe should not be rejected
        let result = reject_interactive_script("vi hello.txt");
        assert!(result.is_none());
    }
}
