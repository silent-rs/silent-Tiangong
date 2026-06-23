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

/// 检测脚本是否使用 stdin 自动化（heredoc/管道/echo 注入）驱动交互程序。
/// 这是反模式：即便交互式终端能力已启用，用 heredoc 自动化 vi 仍不可靠
/// （vi 的 ex 模式行为不稳定、转义复杂），应拒绝并引导 Agent 分步操作。
/// 注意：`|` 检测会匹配任意管道，`vi file | tee backup` 这类合法用法理论上会被误拒。
/// 但本函数仅在 `script_uses_interactive_terminal_program` 也为 true 时才被调用
///（双重条件），实际误判率极低，可接受。
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

/// 拦截 Agent 发起的交互式终端程序。
///
/// 门禁策略随 `interactive` 参数分流：
/// - `interactive=false`（默认，向后兼容）：无条件拒绝所有交互程序。LLM 未显式声明
///   交互意图时，按基础终端分支语义处理，避免误用导致 PTY 卡死。
/// - `interactive=true`：放行裸交互程序（如 `vi file.txt`、`python`），仅拦截
///   stdin 自动化（heredoc/pipe/echo 驱动 vi 等）的反模式。Agent 必须通过
///   `run_shell{interactive:true}` 显式声明，才能启动交互程序并进入 AgentInteractive 态。
fn reject_interactive_script(script: &str, interactive: bool) -> Option<ToolResult> {
    if interactive {
        // 交互模式：只拦截 stdin 自动化反模式，放行裸交互程序
        if script_uses_interactive_terminal_program(script) && script_uses_stdin_automation(script)
        {
            return Some(ToolResult {
                ok: false,
                summary: "不支持用 stdin 自动化驱动交互式终端程序".to_string(),
                stdout: String::new(),
                stderr: "检测到脚本试图通过 heredoc/管道/echo 自动化驱动 vi/vim/nano 等交互程序，这种用法不可靠。请直接以 `run_shell{interactive:true, script:\"vi <file>\"}` 启动，然后在终端面板手动操作，或改用 write_file / replace_in_file 完成编辑。".to_string(),
                exit_code: 2,
                execution: None,
            });
        }
        return None;
    }
    // 非交互模式：无条件拒绝所有交互程序（保持基础终端分支语义）
    if !script_uses_interactive_terminal_program(script) {
        return None;
    }
    Some(ToolResult {
        ok: false,
        summary: "不支持 Agent 自动操作交互式终端程序".to_string(),
        stdout: String::new(),
        stderr: "检测到脚本中包含 vi/vim/nano/ssh/python REPL 等交互式终端程序。如需启动交互程序，请使用 `run_shell{interactive:true, script:\"<command>\"}` 显式声明交互意图。否则请改用 write_file / replace_in_file 完成文件编辑，或使用非交互 shell 命令（如 sed、awk）。".to_string(),
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
        // 读取 interactive 标志：Agent 显式声明要启动交互程序（vi/nano/REPL）。
        // true 时走 exec_interactive 进入 AgentInteractive 态；false 时保持基础终端语义。
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
        let session_id = session_id.to_string();
        Box::pin(async move {
            let selection = match provider.select_for_command(&session_id).await {
                Some(selection) => selection,
                None => return None,
            };
            let terminal_id = selection.terminal_id;

            // 若 agent 指定了 cwd 且与终端当前 cwd 不同，包装为 cd <cwd> && <script>。
            // exec_command trait 方法不携带 cwd，这里手动前置 cd。
            let command = match cwd.as_deref() {
                Some(want) if !want.is_empty() => {
                    let current = provider.current_cwd(&terminal_id).await;
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
                // 交互模式：以 CR 提交命令，等待初始渲染，进入 AgentInteractive 态。
                // wait_secs=3 给 vi/nano 足够时间绘制界面。
                match provider.exec_interactive(&terminal_id, &command, 3).await {
                    Some(r) => r,
                    None => return None, // 终端不可用，回退到默认 run_shell
                }
            } else {
                match provider.exec(&terminal_id, &command, timeout_secs).await {
                    Some(r) => r,
                    None => return None, // 终端不可用，回退到默认 run_shell
                }
            };

            let stdout = Self::truncate_text(&result.stdout, OUTPUT_THRESHOLD);

            // 成功判定随交互模式分流：
            // - 交互模式：interactive_mode=true 是预期的成功（命令已进入交互态），
            //   报告 ok=true。stdout 携带终端首次变化后的完整可见内容（vi 编辑页、
            //   swap 提示 E325、REPL 提示符等），由 Agent 自行阅读判断当前状态。
            // - 非交互模式：interactive_mode=true 意味着命令意外进入交互（兜底检测），
            //   前台进程仍在运行，必须报告失败，避免 Agent 误判后在卡住的 PTY 继续操作。
            let mut summary = if interactive && result.interactive_mode {
                "命令已进入交互态（终端当前显示见 stdout）".to_string()
            } else if result.interactive_mode {
                "命令进入交互模式（未声明 interactive:true）".to_string()
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
            if interactive && result.interactive_mode {
                // stdout 携带终端首次变化后的完整可见内容。引导 Agent 阅读该内容，
                // 据此判断交互程序所处状态（已进入编辑器 / 遇到 swap 提示 / 等待输入
                // 等），并据此引导用户在终端面板手动操作或改用非交互方式。
                stderr.push_str(
                    "\n[提示] stdout 为终端当前显示内容，请阅读判断命令状态：\
若已进入编辑器（vi/nano 编辑界面），可引导用户在其中操作；\
若遇到提示（如 swap 恢复 E325、确认 [Y/n]），引导用户在终端面板按键选择；\
若不便交互，可改用非交互方式（write_file / replace_in_file 等）。",
                );
            } else if result.interactive_mode {
                stderr.push_str(
                    "\n[提示] 命令似乎进入了交互模式（未通过 interactive:true 声明）。\
如需启动交互程序，请使用 `run_shell{interactive:true}`。",
                );
            } else if result.interrupted_by_user {
                stderr.push_str("\n[提示] 命令被用户中断，建议询问用户是否需要调整执行计划");
            }

            // 交互模式且成功进入交互态视为成功
            let ok = if interactive && result.interactive_mode {
                true
            } else {
                result.exit_code == 0 && !result.timed_out && !result.interactive_mode
            };

            Some(ToolResult {
                ok,
                summary,
                stdout,
                stderr,
                exit_code: result.exit_code,
                execution: None,
            })
        })
    }

    /// 处理 terminal_send：向已进入交互态的终端发送按键/文本，返回屏幕新快照。
    ///
    /// 这是 Agent 持续操作交互程序（vi/nano/REPL）的核心工具。每次调用完成
    /// "发送输入 → 等待屏幕变化 → 返回新快照"的原子操作，Agent 据此观察程序
    /// 反应并决定下一步输入，形成持续交互闭环。
    fn handle_terminal_send(
        provider: &std::sync::Arc<dyn tiangong_core::terminal_trait::TerminalProvider>,
        call: &tiangong_core::model::ToolCall,
        session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        // input：要发送的按键/文本（原样传递，转义由终端执行侧 command_protocol 处理）。
        let input = match call.arguments.get("input").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return Box::pin(async {
                    Some(ToolResult {
                        ok: false,
                        summary: "terminal_send 缺少 input 参数".to_string(),
                        stdout: String::new(),
                        stderr: "input 参数为必填，指定要发送到终端的按键/文本".to_string(),
                        exit_code: 2,
                        execution: None,
                    })
                });
            }
        };
        // wait_secs：发送后等待屏幕变化的秒数（默认 3，给程序渲染时间）
        let wait_secs = call
            .arguments
            .get("wait")
            .and_then(|v| v.as_u64())
            .unwrap_or(3);

        let provider = provider.clone();
        let session_id = session_id.to_string();
        Box::pin(async move {
            let result = match provider
                .send_interactive(&session_id, &input, wait_secs)
                .await
            {
                Some(r) => r,
                None => return None, // 终端不可用，回退到默认逻辑
            };

            let stdout = Self::truncate_text(&result.stdout, OUTPUT_THRESHOLD);
            let summary = if result.interactive_mode {
                "终端已更新（当前显示见 stdout）".to_string()
            } else if result.exit_code != 0 {
                format!("终端输入失败（退出码 {}）", result.exit_code)
            } else {
                "终端输入已发送".to_string()
            };

            Some(ToolResult {
                // ok 与 exit_code 一致：send_interactive 在 PTY 不可用或写入失败时
                // 返回 exit_code: -1，此时 ok 必须为 false，否则 Agent 会误以为成功
                ok: result.exit_code == 0,
                summary,
                stdout,
                stderr: String::new(),
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
            "terminal_send" => Self::handle_terminal_send(&self.provider, call, session_id),
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
            "需要启动交互式终端程序（如 vi/vim/nano 编辑文件、python/node REPL）时，使用 `run_shell{interactive:true, script:\"<command>\"}` 显式声明。命令在终端首次渲染屏幕后返回，stdout 为终端当前显示内容（已过滤控制序列）。请阅读该内容判断命令状态，然后用 `terminal_send{input:\"<按键>\"}` 持续操作终端：每次发送按键后自动等待屏幕变化并返回新快照，据此观察程序反应决定下一步。例如遇到 vi 的 swap 恢复提示（E325）时，发 `terminal_send{input:\"d\"}` 删除 swap 文件，返回的快照会显示进入编辑器后的界面；在 vi 中编辑完成后发 `terminal_send{input:\"\\x1b:wq\\r\"}` 保存退出。禁止用 heredoc/管道/echo 自动化驱动交互程序。".to_string(),
            "文件编辑优先使用 write_file / replace_in_file；仅当用户明确要求在终端编辑器（如 vi）中操作，或需要交互式程序（如 REPL 实验）时才用 `run_shell{interactive:true}`。".to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_heredoc_driven_vi_non_interactive() {
        let script = "rm -f hello.txt\nvi -es hello.txt <<'EOF'\ni\nworld\n.\nwq\nEOF\n";
        // 非交互模式：无条件拒绝
        let result = reject_interactive_script(script, false);
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(!result.ok);
        assert_eq!(result.exit_code, 2);
    }

    #[test]
    fn rejects_heredoc_driven_vi_even_interactive() {
        // 交互模式下，stdin 自动化驱动 vi 仍被拒（反模式）
        let script = "rm -f hello.txt\nvi -es hello.txt <<'EOF'\ni\nworld\n.\nwq\nEOF\n";
        let result = reject_interactive_script(script, true);
        assert!(result.is_some());
    }

    #[test]
    fn rejects_expect_driven_vi() {
        let script =
            "expect <<'EOF'\nset timeout 10\nspawn vi hello.txt\nsend \"iworld\\033:wq\\r\"\nexpect eof\nEOF";
        // 两种模式都拒（heredoc 自动化）
        assert!(reject_interactive_script(script, false).is_some());
        assert!(reject_interactive_script(script, true).is_some());
    }

    #[test]
    fn rejects_pipe_driven_vi() {
        let script = "printf 'iworld\\033:wq\\n' | vi hello.txt";
        // 管道自动化：两种模式都拒
        assert!(reject_interactive_script(script, false).is_some());
        assert!(reject_interactive_script(script, true).is_some());
    }

    #[test]
    fn allows_regular_shell_script() {
        // 普通脚本两种模式都放行
        assert!(reject_interactive_script("rm -f hello.txt\ncat hello.txt", false).is_none());
        assert!(reject_interactive_script("rm -f hello.txt\ncat hello.txt", true).is_none());
    }

    #[test]
    fn rejects_plain_vi_non_interactive() {
        // 非交互模式：裸 vi 被拒（未声明交互意图）
        let result = reject_interactive_script("vi hello.txt", false);
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(!result.ok);
        assert_eq!(result.exit_code, 2);
    }

    #[test]
    fn allows_plain_vi_interactive() {
        // 交互模式：裸 vi 放行（Agent 显式声明交互意图）
        let result = reject_interactive_script("vi hello.txt", true);
        assert!(result.is_none());
    }

    #[test]
    fn allows_python_repl_interactive() {
        // 交互模式放行 REPL
        assert!(reject_interactive_script("python", true).is_none());
        assert!(reject_interactive_script("python3", true).is_none());
    }

    #[test]
    fn rejects_python_repl_non_interactive() {
        let result = reject_interactive_script("python", false);
        assert!(result.is_some());
    }

    #[test]
    fn allows_ssh_interactive() {
        assert!(reject_interactive_script("ssh user@host", true).is_none());
    }

    #[test]
    fn rejects_ssh_non_interactive() {
        let result = reject_interactive_script("ssh user@host", false);
        assert!(result.is_some());
    }
}
