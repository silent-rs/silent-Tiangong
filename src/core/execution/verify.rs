use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, path::PathBuf};

use anyhow::{Context, Result};

use crate::core::agents::response_agent::VerifyExecutionRecord;

pub fn recommend_verify_commands(user_input: &str) -> Vec<String> {
    let text = user_input.to_ascii_lowercase();
    let likely_code_task = contains_any(
        &text,
        &[
            "rust", ".rs", "代码", "编译", "构建", "check", "clippy", "cargo", "修复", "重构",
        ],
    );
    if !likely_code_task {
        return Vec::new();
    }

    let mut commands = vec!["cargo check --workspace".to_string()];
    if contains_any(&text, &["clippy", "严格", "-d warnings"]) {
        commands.push(
            "cargo clippy --workspace --all-targets --tests --benches -- -D warnings".to_string(),
        );
    }
    commands
}

pub fn run_verify_commands(commands: &[String]) -> Vec<VerifyExecutionRecord> {
    let mut records = Vec::new();
    let timeout_ms = verify_command_timeout_ms();

    for command in commands
        .iter()
        .map(|cmd| cmd.trim())
        .filter(|cmd| !cmd.is_empty() && *cmd != "无")
    {
        let parts = command
            .split_whitespace()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let Some(program) = parts.first().cloned() else {
            continue;
        };
        let args = parts.iter().skip(1).cloned().collect::<Vec<_>>();

        let started = Instant::now();
        if !is_allowed_verify_command(&program, &args) {
            records.push(VerifyExecutionRecord {
                command: command.to_string(),
                ok: false,
                exit_code: 1,
                duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                summary: format!("验证命令不在允许列表：{command}"),
                stdout: String::new(),
                stderr: String::new(),
            });
            continue;
        }

        let workspace = match std::env::current_dir() {
            Ok(path) => path,
            Err(err) => {
                records.push(VerifyExecutionRecord {
                    command: command.to_string(),
                    ok: false,
                    exit_code: 1,
                    duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                    summary: format!("读取工作目录失败：{err}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                });
                continue;
            }
        };

        let outcome = execute_command_with_timeout(
            Command::new(&program)
                .args(&args)
                .current_dir(workspace)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        )
        .with_context(|| format!("执行验证命令失败：{command}"));

        match outcome {
            Ok((output, timed_out)) => {
                let mut exit_code = output.status.code().unwrap_or(-1);
                if timed_out {
                    exit_code = -1;
                }
                let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
                let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
                let ok = !timed_out && output.status.success();
                let summary = if timed_out {
                    format!("验证超时：{command} (timeout_ms={timeout_ms})")
                } else if ok {
                    format!("验证通过：{command}")
                } else {
                    let detail = extract_actionable_error(&stderr, &stdout)
                        .unwrap_or_else(|| "无错误详情".to_string());
                    format!("验证失败：{command} (exit_code={exit_code})，建议先处理：{detail}")
                };

                records.push(VerifyExecutionRecord {
                    command: command.to_string(),
                    ok,
                    exit_code,
                    duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                    summary,
                    stdout,
                    stderr,
                });
            }
            Err(err) => {
                records.push(VerifyExecutionRecord {
                    command: command.to_string(),
                    ok: false,
                    exit_code: 1,
                    duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                    summary: format!("验证命令执行异常：{command}"),
                    stdout: String::new(),
                    stderr: err.to_string(),
                });
            }
        }
    }

    records
}

fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| text.contains(keyword))
}

fn verify_command_timeout_ms() -> u64 {
    const DEFAULT_TIMEOUT_MS: u64 = 120_000;
    std::env::var("VERIFY_COMMAND_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
}

fn is_allowed_verify_command(program: &str, args: &[String]) -> bool {
    if program == "cargo" {
        return args
            .first()
            .map(|sub| matches!(sub.as_str(), "check" | "clippy" | "build" | "test"))
            .unwrap_or(false);
    }
    if matches!(program, "cat" | "head" | "tail" | "wc" | "ls") {
        return validate_verify_paths(args);
    }
    false
}

fn validate_verify_paths(args: &[String]) -> bool {
    let workspace_root = match env::current_dir() {
        Ok(path) => normalize_path(path),
        Err(_) => return false,
    };
    let temp_root = normalize_path(env::temp_dir());

    for arg in args {
        let trimmed = arg.trim();
        if trimmed.is_empty() || trimmed.starts_with('-') {
            continue;
        }
        if trimmed.parse::<i64>().is_ok() {
            continue;
        }

        let path = normalize_path(if PathBuf::from(trimmed).is_absolute() {
            PathBuf::from(trimmed)
        } else {
            workspace_root.join(trimmed)
        });
        if !(path.starts_with(&workspace_root) || path.starts_with(&temp_root)) {
            return false;
        }
    }
    true
}

fn normalize_path(path: PathBuf) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn execute_command_with_timeout(command: &mut Command, timeout_ms: u64) -> Result<(Output, bool)> {
    let mut child = command.spawn().context("spawn 子进程失败")?;
    let timeout = Duration::from_millis(timeout_ms);
    let started = Instant::now();

    loop {
        if let Some(_status) = child.try_wait().context("轮询子进程状态失败")? {
            let output = child.wait_with_output().context("读取命令输出失败")?;
            return Ok((output, false));
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            let output = child.wait_with_output().context("读取超时命令输出失败")?;
            return Ok((output, true));
        }

        thread::sleep(Duration::from_millis(20));
    }
}

fn extract_actionable_error(stderr: &str, stdout: &str) -> Option<String> {
    for raw in stderr.lines().chain(stdout.lines()) {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains("error")
            || line.contains("Error")
            || line.contains("failed")
            || line.contains("warning:")
        {
            return Some(line.chars().take(220).collect());
        }
    }
    None
}

fn truncate_output(raw: &str) -> String {
    const MAX_CHARS: usize = 4000;
    let mut output = raw.chars().take(MAX_CHARS).collect::<String>();
    if raw.chars().count() > MAX_CHARS {
        output.push_str("\n...(truncated)");
    }
    output
}

fn elapsed_ms_u64(raw: u128) -> u64 {
    raw.min(u64::MAX as u128) as u64
}
