use std::sync::Arc;

use tiangong_core::terminal_trait::TerminalProvider;
use tiangong_core::tool::ToolResult;

use crate::util::shell_quote;

const OUTPUT_THRESHOLD: usize = 2000;

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

/// 拦截 Agent 发起的交互式终端程序。
///
/// 当前基础终端分支不支持 Agent 自动操作交互式终端程序
/// （terminal_input/output/reset 等交互能力在独立分支开发）。
/// 一旦检测到脚本中出现 vi/vim/nano/ssh/python REPL 等交互程序，
/// 无论是否伴随 stdin 自动化，都直接拒绝，避免 PTY 进入前台交互后卡死。
fn reject_interactive_script(script: &str) -> Option<ToolResult> {
    if !script_uses_interactive_terminal_program(script) {
        return None;
    }
    Some(ToolResult {
        ok: false,
        summary: "不支持 Agent 自动操作交互式终端程序".to_string(),
        stdout: String::new(),
        stderr: "检测到脚本中包含 vi/vim/nano/ssh/python REPL 等交互式终端程序。当前基础终端分支不支持 Agent 自动操作交互式终端程序，请改用 write_file / replace_in_file 完成文件编辑，或使用非交互 shell 命令（如 sed、awk）。交互式终端能力在独立分支开发。".to_string(),
        exit_code: 2,
        execution: None,
    })
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
        session_id: &str,
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
        let session_id = session_id.to_string();
        Box::pin(async move {
            // 若 agent 指定了 cwd 且与终端当前 cwd 不同，包装为 cd <cwd> && <script>。
            // exec_command trait 方法不携带 cwd，这里手动前置 cd。
            let command = match cwd.as_deref() {
                Some(want) if !want.is_empty() => {
                    let current = provider.current_cwd(&session_id).await;
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
            let result = match provider.exec(&session_id, &command, timeout_secs).await {
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
        session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        match call.name.as_str() {
            "run_shell" => Self::handle_run_shell(&self.provider, call, session_id),
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
            "当前基础终端能力不支持 Agent 自动操作交互式终端程序（如 vi/vim/nano/ssh/python REPL）。遇到文件编辑应使用 write_file / replace_in_file，或使用非交互 shell 命令（如 sed、awk）完成。".to_string(),
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
    fn rejects_plain_vi() {
        // 裸 vi 调用（无 heredoc/pipe）也应被拒绝：当前分支不支持 Agent 操作交互式程序
        let result = reject_interactive_script("vi hello.txt");
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(!result.ok);
        assert_eq!(result.exit_code, 2);
    }

    #[test]
    fn rejects_python_repl() {
        let result = reject_interactive_script("python");
        assert!(result.is_some());
    }

    #[test]
    fn rejects_node_repl() {
        let result = reject_interactive_script("node");
        assert!(result.is_some());
    }

    #[test]
    fn rejects_ssh() {
        let result = reject_interactive_script("ssh user@host");
        assert!(result.is_some());
    }
}
