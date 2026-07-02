use std::sync::Arc;

use tiangong_core::terminal_trait::TerminalProvider;
use tiangong_core::tool::ToolResult;

use crate::util::shell_quote;

const OUTPUT_THRESHOLD: usize = 2000;

/// 终端工具覆盖处理器（run_command / run_shell / terminal_send）。
///
/// run_command 与 run_shell 都经 PTY 执行，输出回显到嵌入式终端面板。run_command
/// 额外做命令白名单 / 路径越界校验（与 core 原 LocalToolExecutor 行为一致），
/// run_shell 直接执行脚本（保持原有语义）。
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

    /// run_command：校验后经 PTY 执行受控命令。
    fn handle_run_command(
        provider: &Arc<dyn TerminalProvider>,
        call: &tiangong_core::model::ToolCall,
        session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let raw_cmd = match call.arguments.get("cmd").and_then(|v| v.as_str()) {
            Some(c) => c.trim().to_string(),
            None => {
                return Box::pin(async {
                    Some(ToolResult {
                        ok: false,
                        summary: "run_command 缺少 cmd 参数".to_string(),
                        stdout: String::new(),
                        stderr: "cmd 参数为必填".to_string(),
                        exit_code: 2,
                        execution: None,
                    })
                });
            }
        };
        if raw_cmd.is_empty() {
            return Box::pin(async {
                Some(ToolResult {
                    ok: false,
                    summary: "run_command cmd 不能为空".to_string(),
                    stdout: String::new(),
                    stderr: "cmd 不能为空".to_string(),
                    exit_code: 2,
                    execution: None,
                })
            });
        }

        // 拆分命令 + 收集 args
        let (cmd, mut args) = split_cmd(&raw_cmd);
        if let Some(arr) = call.arguments.get("args").and_then(|v| v.as_array()) {
            args.extend(
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(ToString::to_string),
            );
        }
        let cwd = call
            .arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let timeout_secs = call
            .arguments
            .get("timeout")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .filter(|v| *v > 0);

        // 命令白名单 / 路径校验（FullTrust 时跳过——由 engine 的 trust_mode 决定，
        // 此处保守校验；终端插件无 trust_mode 引用，始终校验）
        if let Err(e) = validate_terminal_command(&cmd, &args, cwd.as_deref()) {
            let msg = e.to_string();
            return Box::pin(async move {
                Some(ToolResult {
                    ok: false,
                    summary: format!("run_command 校验失败：{msg}"),
                    stdout: String::new(),
                    stderr: msg,
                    exit_code: 1,
                    execution: None,
                })
            });
        }

        // 拼装 PTY 命令字符串：cmd + quoted args
        let mut command = cmd.clone();
        for arg in &args {
            command.push(' ');
            command.push_str(&shell_quote(arg));
        }

        let provider = provider.clone();
        let session_id = session_id.to_string();
        Box::pin(async move {
            let selection = match provider.select_for_command(&session_id).await {
                Some(s) => s,
                None => return None,
            };
            let terminal_id = selection.terminal_id.clone();

            // cwd 包装
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

            let result = match provider.exec(&terminal_id, &command, timeout_secs).await {
                Some(r) => r,
                None => return None,
            };

            let stdout = Self::truncate_text(&result.stdout, OUTPUT_THRESHOLD);
            let mut summary = if result.interactive_mode {
                "命令进入交互模式（未声明 interactive:true）".to_string()
            } else if result.timed_out {
                "命令执行超时".to_string()
            } else if result.exit_code != 0 {
                format!("命令执行失败（退出码 {}）", result.exit_code)
            } else {
                "命令执行成功".to_string()
            };
            if !result.cwd_after.is_empty() {
                summary.push_str(&format!("（cwd: {}）", result.cwd_after));
            }
            summary.push_str(&selection.feedback_text());

            let ok = result.exit_code == 0 && !result.timed_out && !result.interactive_mode;

            Some(ToolResult {
                ok,
                summary,
                stdout,
                stderr: result.stderr,
                exit_code: result.exit_code,
                execution: None,
            })
        })
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
        // 读取 interactive 标志：Agent 显式声明要启动交互程序。
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

        let provider = provider.clone();
        let session_id = session_id.to_string();
        Box::pin(async move {
            let selection = match provider.select_for_command(&session_id).await {
                Some(selection) => selection,
                None => return None,
            };
            let terminal_id = selection.terminal_id.clone();

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
                // wait_secs=3 给交互程序足够时间绘制首屏。
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
            //   报告 ok=true。stdout 携带终端首次变化后的完整可见内容，
            //   由 Agent 自行阅读判断当前状态。
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
            summary.push_str(&selection.feedback_text());

            let mut stderr = result.stderr.clone();
            if interactive && result.interactive_mode {
                // stdout 携带终端首次变化后的完整可见内容。Agent 阅读该内容后，
                // 使用 terminal_send 继续输入或提示用户在终端面板操作。
                stderr.push_str(
                    "\n[提示] stdout 为终端当前显示内容，请阅读判断命令状态：\
若程序等待输入，请使用 `terminal_send` 向同一终端继续发送按键或文本；\
若需要用户亲自操作，可引导用户打开终端面板处理。",
                );
            } else if result.interactive_mode {
                stderr.push_str(
                    "\n[提示] 命令似乎进入了交互模式（未通过 interactive:true 声明）。\
请阅读 stdout 中的终端当前显示内容，并使用 `terminal_send` 向同一终端继续发送按键或文本。\
后续如需主动启动交互程序，请优先使用 `run_shell{interactive:true}`。",
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
    /// 这是 Agent 持续操作交互程序的核心工具。每次调用完成
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
        session: &tiangong_core::session::Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let session_id = session.id.as_str();
        match call.name.as_str() {
            "run_command" => Self::handle_run_command(&self.provider, call, session_id),
            "run_shell" => Self::handle_run_shell(&self.provider, call, session_id),
            "terminal_send" => Self::handle_terminal_send(&self.provider, call, session_id),
            _ => Box::pin(async { None }),
        }
    }
}

/// 拆分命令字符串为 (程序名, 参数列表)。
fn split_cmd(raw: &str) -> (String, Vec<String>) {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !in_single => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        return (raw.to_string(), Vec::new());
    }
    let cmd = parts.remove(0);
    (cmd, parts)
}

/// 校验命令（白名单 + 路径越界），与 core 原 LocalToolExecutor 行为一致。
fn validate_terminal_command(cmd: &str, args: &[String], cwd: Option<&str>) -> anyhow::Result<()> {
    use tiangong_core::tool::common as shared;
    // 解析 cwd 为 effective_cwd（用于路径校验）
    let base = if let Some(cwd) = cwd.filter(|s| !s.is_empty()) {
        std::path::PathBuf::from(cwd)
    } else {
        shared::workspace_root()?
    };
    let effective_cwd = if base.is_dir() {
        base.canonicalize().unwrap_or(base)
    } else {
        base
    };
    if matches!(cmd, "bash" | "sh" | "powershell" | "pwsh") {
        shared::validate_shell_command_args(cmd, args, &effective_cwd)?;
    } else {
        if !shared::is_allowed_command(cmd) {
            return Err(anyhow::anyhow!("不允许执行命令：{cmd}"));
        }
        shared::validate_command_args_in_allowed_roots(cmd, args, &effective_cwd)?;
    }
    Ok(())
}

/// 终端 Prompt 规则提供者：注入基础命令执行规则
pub struct TerminalPromptSectionProvider;

impl tiangong_core::tool_override::PromptSectionProvider for TerminalPromptSectionProvider {
    fn prompt_sections(&self) -> Vec<String> {
        vec![
            "当用户请求涉及命令行操作（安装依赖、创建项目、编译构建、文件操作等），必须逐步使用 `run_shell` 实际执行命令，不要仅以文本或代码块形式展示命令给用户。每步执行一条命令，根据执行结果决定下一步操作。回复中可以用代码块展示已执行的命令，但必须先通过 `run_shell` 实际执行。".to_string(),
            "如果命令会启动需要持续输入的交互程序（例如编辑器、REPL、TUI、远程会话、确认流程等），使用 `run_shell{interactive:true, script:\"<command>\"}` 显式声明交互意图。返回的 stdout 是终端当前显示内容，请阅读后用 `terminal_send{input:\"<按键>\"}` 持续操作；每次发送后都会返回新快照。".to_string(),
            "文件编辑优先使用 write_file / replace_in_file；只有用户明确要求在终端程序里操作，或确实需要交互式程序时才用 `run_shell{interactive:true}`。".to_string(),
        ]
    }
}
