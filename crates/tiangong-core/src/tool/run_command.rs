use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::process::Command;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use crate::agent_config::AgentConfig;
use crate::process::configure_tokio_no_window;

use super::common::{
    command_env_allowlist, command_timeout_ms, derive_shell_exec_args, is_allowed_command,
    resolve_effective_cwd, truncate_output, validate_command_args_in_allowed_roots,
    validate_shell_command_args,
};
use super::{LocalToolExecutor, ToolCall, ToolResult};

const INTERNAL_SHELL_CMD: &str = "__tiangong_shell__";
const INTERNAL_CWD_PREFIX: &str = "__tiangong_cwd=";

pub(super) fn collect_runtime_env(agent_config: &AgentConfig) -> BTreeMap<String, String> {
    let mut runtime_env = BTreeMap::new();

    if agent_config.mcp.enabled {
        for server in &agent_config.mcp.servers {
            if !server.enabled {
                continue;
            }
            for (key, value) in &server.env {
                let key = key.trim();
                if !is_valid_env_key(key) {
                    continue;
                }
                runtime_env.insert(key.to_string(), value.trim().to_string());
            }
        }
    }

    if agent_config.skills.enabled {
        for skill in &agent_config.skills.installed {
            if !skill.enabled {
                continue;
            }
            let source = skill.source.value.trim();
            if source.is_empty() {
                continue;
            }
            let source_path = Path::new(source);
            let skill_dir = if source_path.is_dir() {
                source_path
            } else if let Some(parent) = source_path.parent() {
                parent
            } else {
                continue;
            };
            for (key, value) in load_local_env(skill_dir) {
                runtime_env.insert(key, value);
            }
        }
    }

    runtime_env
}

impl LocalToolExecutor {
    pub(super) fn run_command(&self, call: &ToolCall) -> Result<ToolResult> {
        let raw_cmd = call
            .args
            .first()
            .ok_or_else(|| anyhow!("run_command 缺少命令参数"))?
            .to_string();
        let mut raw_args = call.args.iter().skip(1).cloned().collect::<Vec<_>>();
        let cwd = extract_cwd_meta(&mut raw_args);
        let effective_cwd = resolve_effective_cwd(cwd.as_deref())?;

        let (cmd, mut args) = if raw_cmd == INTERNAL_SHELL_CMD {
            let script = raw_args
                .first()
                .map(String::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| anyhow!("run_shell 缺少 script 参数"))?;
            let shell = raw_args.get(1).map(String::as_str);
            derive_shell_exec_args(script, shell)?
        } else {
            (raw_cmd.clone(), raw_args)
        };

        // 提取 LLM 指定的超时（在 validate 之前，避免被当成路径参数）
        let timeout_ms = extract_timeout_meta(&mut args).unwrap_or_else(command_timeout_ms);
        // 信任模式下跳过命令白名单和路径安全检查
        if !self.is_full_trust() {
            if matches!(cmd.as_str(), "bash" | "sh" | "powershell" | "pwsh") {
                validate_shell_command_args(&cmd, &args, &effective_cwd)?;
            } else {
                if !is_allowed_command(&cmd) {
                    return Err(anyhow!("不允许执行命令：{cmd}"));
                }
                validate_command_args_in_allowed_roots(&cmd, &args, &effective_cwd)?;
            }
        }

        // 校验通过后，有 PTY provider 则走终端执行（输出出现在嵌入式终端面板）
        if let Some(provider) = self.terminal_provider() {
            return self.run_command_via_pty(provider, &cmd, &args, &effective_cwd, timeout_ms);
        }

        let env_allowlist = command_env_allowlist();
        let runtime_env = self.runtime_env();
        let file_env = load_local_env(&effective_cwd);

        let output = run_command_async(
            &cmd,
            &args,
            &effective_cwd,
            &env_allowlist,
            runtime_env.clone(),
            &file_env,
            timeout_ms,
        )?;

        let (output, timed_out) = match output {
            Ok(Ok(payload)) => (payload, false),
            Ok(Err(err)) => return Err(anyhow!("执行命令失败：{cmd}，{err}")),
            Err(_) => {
                return Ok(ToolResult {
                    ok: false,
                    summary: format!("命令执行超时：{cmd} (timeout_ms={timeout_ms})"),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                    execution: None,
                });
            }
        };

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = output.status.success() && !timed_out;
        let summary = if ok {
            format!("命令执行成功：{cmd}")
        } else {
            format!("命令执行失败：{cmd} (exit_code={exit_code})")
        };

        Ok(ToolResult {
            ok,
            summary,
            stdout,
            stderr,
            exit_code,
            execution: None,
        })
    }

    /// 通过 PTY 执行命令（校验已通过）。
    ///
    /// 与独立子进程不同，PTY 路径会把命令输出回显到嵌入式终端面板，
    /// 让用户能看到 agent 正在做什么。命令白名单/路径校验已在 `run_command` 中完成。
    ///
    /// cwd 处理：`exec_command` trait 方法不携带 cwd，因此当 `effective_cwd`
    /// 与终端当前 cwd 不同时，把命令包装为 `cd <cwd> && <cmd> <args>`，
    /// 确保 agent 指定的工作目录被尊重（修复集成分支丢失 cwd 的缺陷）。
    fn run_command_via_pty(
        &self,
        provider: &std::sync::Arc<dyn crate::terminal_trait::TerminalProvider>,
        cmd: &str,
        args: &[String],
        effective_cwd: &std::path::Path,
        timeout_ms: u64,
    ) -> Result<ToolResult> {
        let timeout_secs = if timeout_ms > 0 {
            Some(timeout_ms / 1000)
        } else {
            None
        };

        let result = exec_via_pty(provider, cmd, args, effective_cwd, timeout_secs)?;

        match result {
            Some(r) => {
                let stdout = truncate_output(&r.stdout);
                let stderr = truncate_output(&r.stderr);
                let ok = !r.timed_out && (r.interactive_mode || r.exit_code == 0);
                let summary = if ok {
                    format!("命令执行成功：{cmd}")
                } else if r.timed_out {
                    format!("命令执行超时：{cmd}")
                } else {
                    format!("命令执行失败：{cmd} (exit_code={})", r.exit_code)
                };
                Ok(ToolResult {
                    ok,
                    summary,
                    stdout,
                    stderr,
                    exit_code: r.exit_code,
                    execution: None,
                })
            }
            None => Err(anyhow!("终端会话不可用")),
        }
    }
}

fn extract_cwd_meta(args: &mut Vec<String>) -> Option<String> {
    let mut cwd = None;
    args.retain(|arg| {
        if let Some(value) = arg.strip_prefix(INTERNAL_CWD_PREFIX) {
            cwd = Some(value.to_string());
            false
        } else {
            true
        }
    });
    cwd
}

fn load_local_env(cwd: &Path) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for file in [".env.local", ".env"] {
        let path = cwd.join(file);
        if !path.is_file() {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if !is_valid_env_key(key) {
                continue;
            }
            let value = normalize_env_value(value.trim());
            env.push((key.to_string(), value));
        }
    }
    env
}

fn is_valid_env_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    for (idx, ch) in key.chars().enumerate() {
        if idx == 0 && !(ch.is_ascii_alphabetic() || ch == '_') {
            return false;
        }
        if !(ch.is_ascii_alphanumeric() || ch == '_') {
            return false;
        }
    }
    true
}

fn normalize_env_value(raw: &str) -> String {
    let mut value = raw.trim().to_string();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0] as char;
        let last = bytes[value.len() - 1] as char;
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            value = value[1..value.len() - 1].to_string();
        }
    }
    value
}

const INTERNAL_TIMEOUT_PREFIX: &str = "__tiangong_timeout=";

/// 从参数列表中提取 __tiangong_timeout=N 元数据，返回超时毫秒数
fn extract_timeout_meta(args: &mut Vec<String>) -> Option<u64> {
    let idx = args
        .iter()
        .position(|a| a.starts_with(INTERNAL_TIMEOUT_PREFIX))?;
    let raw = args.remove(idx);
    raw[INTERNAL_TIMEOUT_PREFIX.len()..]
        .trim()
        .parse::<u64>()
        .ok()
}

type CommandOutput = Result<std::process::Output, std::io::Error>;

fn run_command_async(
    cmd: &str,
    args: &[String],
    cwd: &Path,
    env_allowlist: &[(String, String)],
    runtime_env: BTreeMap<String, String>,
    file_env: &[(String, String)],
    timeout_ms: u64,
) -> anyhow::Result<Result<CommandOutput, tokio::time::error::Elapsed>> {
    let cmd = cmd.to_string();
    let args = args.to_vec();
    let cwd = cwd.to_path_buf();
    let env_allowlist = env_allowlist.to_vec();
    let file_env = file_env.to_vec();

    let handle = tokio::runtime::Handle::try_current();
    match handle {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| {
                h.block_on(exec_command(
                    &cmd,
                    &args,
                    &cwd,
                    &env_allowlist,
                    runtime_env,
                    &file_env,
                    timeout_ms,
                ))
            })
        }
        _ => {
            // current_thread 运行时或无运行时：在新线程中创建独立 runtime，
            // 避免在 current_thread runtime 内调用 block_on 导致 panic
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        let rt = TokioRuntimeBuilder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("初始化命令执行运行时失败");
                        rt.block_on(exec_command(
                            &cmd,
                            &args,
                            &cwd,
                            &env_allowlist,
                            runtime_env,
                            &file_env,
                            timeout_ms,
                        ))
                    })
                    .join()
                    .expect("命令执行线程 panic")
            })
        }
    }
}

async fn exec_command(
    cmd: &str,
    args: &[String],
    cwd: &Path,
    env_allowlist: &[(String, String)],
    runtime_env: BTreeMap<String, String>,
    file_env: &[(String, String)],
    timeout_ms: u64,
) -> anyhow::Result<Result<CommandOutput, tokio::time::error::Elapsed>> {
    let mut command = Command::new(cmd);
    configure_tokio_no_window(&mut command);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for (key, value) in env_allowlist {
        command.env(key, value);
    }
    for (key, value) in runtime_env {
        command.env(key, value);
    }
    for (key, value) in file_env {
        command.env(key, value);
    }
    if timeout_ms > 0 {
        Ok(timeout(Duration::from_millis(timeout_ms), command.output()).await)
    } else {
        Ok(Ok(command.output().await))
    }
}

// ===== PTY 路由辅助：校验通过后把命令经嵌入式终端执行 =====

/// 需要单引号包裹的 shell 元字符集合（含空白、引号、转义、通配、控制操作符等）。
///
/// 用 `&str::contains` 表达，避免在 `matches!(c, '"' | '\\' | ...)` 里写
/// 带单引号/反斜杠的字符字面量——这类字面量在编辑往返中易被破坏。
const SHELL_METACHARS: &str = " \t\n\r\"$`!*?[](){}|&;<>~'\\";

/// 判断字符是否为需要单引号包裹的 shell 元字符
fn is_shell_metachar(c: char) -> bool {
    c.is_whitespace() || SHELL_METACHARS.contains(c)
}

/// 对单个 shell 参数做单引号转义（与终端插件 shell_quote 一致）
fn shell_quote(s: &str) -> String {
    if s.is_empty() || s.contains(is_shell_metachar) {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    } else {
        s.to_string()
    }
}

/// 将 `cd <cwd> && <cmd> <args>` 包装为一条命令字符串，仅在 cwd 与终端
/// 当前 cwd 不同时包装。返回 (命令字符串, 用于 provider 的 args)。
fn build_pty_command_with_cwd(
    provider: &std::sync::Arc<dyn crate::terminal_trait::TerminalProvider>,
    cmd: &str,
    args: &[String],
    effective_cwd: &Path,
) -> (String, Vec<String>) {
    // 判断是否需要切到 effective_cwd：解析 PTY 当前 cwd，不同则前置 cd。
    let current = pty_current_cwd_blocking(provider);
    let need_cd = match (current.as_deref(), effective_cwd.to_str()) {
        (Some(cur), Some(want)) => !cur
            .trim_end_matches('/')
            .eq_ignore_ascii_case(want.trim_end_matches('/')),
        (None, Some(_)) => true,
        _ => false,
    };

    if need_cd {
        // 包装成单条 shell 命令：cd <cwd> && <cmd> <args>
        let mut parts = vec![format!(
            "cd {}",
            shell_quote(&effective_cwd.to_string_lossy())
        )];
        parts.push(cmd.to_string());
        for arg in args {
            parts.push(shell_quote(arg));
        }
        (parts.join(" && "), Vec::new())
    } else {
        (cmd.to_string(), args.to_vec())
    }
}

/// 阻塞读取 PTY 当前 cwd（用于决定是否需要前置 cd）
fn pty_current_cwd_blocking(
    provider: &std::sync::Arc<dyn crate::terminal_trait::TerminalProvider>,
) -> Option<String> {
    let handle = tokio::runtime::Handle::try_current();
    match handle {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| h.block_on(provider.current_cwd()))
        }
        _ => std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let rt = TokioRuntimeBuilder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("初始化 PTY cwd 读取运行时失败");
                    rt.block_on(provider.current_cwd())
                })
                .join()
                .expect("PTY cwd 读取线程 panic")
        }),
    }
}

/// 通过 PTY 执行命令（非交互）
fn exec_via_pty(
    provider: &std::sync::Arc<dyn crate::terminal_trait::TerminalProvider>,
    cmd: &str,
    args: &[String],
    effective_cwd: &Path,
    timeout_secs: Option<u64>,
) -> anyhow::Result<Option<crate::terminal_trait::TerminalExecResult>> {
    let (pty_cmd, pty_args) = build_pty_command_with_cwd(provider, cmd, args, effective_cwd);
    let provider = provider.clone();

    let handle = tokio::runtime::Handle::try_current();
    match handle {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            Ok(tokio::task::block_in_place(|| {
                h.block_on(provider.exec_command(&pty_cmd, &pty_args, timeout_secs))
            }))
        }
        _ => Ok(std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let rt = TokioRuntimeBuilder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("初始化 PTY 执行运行时失败");
                    rt.block_on(provider.exec_command(&pty_cmd, &pty_args, timeout_secs))
                })
                .join()
                .expect("PTY 执行线程 panic")
        })),
    }
}
