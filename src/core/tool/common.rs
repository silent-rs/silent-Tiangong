use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

pub(super) fn workspace_root() -> Result<PathBuf> {
    std::env::current_dir().context("读取当前工作目录失败")
}

fn allowed_roots() -> Result<Vec<PathBuf>> {
    let workspace = workspace_root()?;
    let workspace_canonical = workspace
        .canonicalize()
        .with_context(|| format!("解析工作目录失败：{}", workspace.display()))?;
    let temp = std::env::temp_dir();
    let temp_canonical = temp.canonicalize().unwrap_or(temp);

    let mut roots = vec![workspace_canonical];
    if !roots.iter().any(|root| root == &temp_canonical) {
        roots.push(temp_canonical);
    }
    Ok(roots)
}

pub(super) fn resolve_workspace_path(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }
    if raw.starts_with('~') {
        return Err(anyhow!(
            "不允许使用 home 路径：{raw}；仅允许当前目录与临时目录"
        ));
    }

    let root = workspace_root()?;
    let candidate = resolve_path_candidate(raw, &root);
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("解析路径失败：{}", candidate.display()))?;
    let roots = allowed_roots()?;

    if !is_path_in_allowed_roots(&canonical, &roots) {
        return Err(anyhow!(
            "路径越界，仅允许当前目录或临时目录：{}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

pub(super) fn resolve_workspace_write_path(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }
    if raw.starts_with('~') {
        return Err(anyhow!(
            "不允许使用 home 路径：{raw}；仅允许当前目录与临时目录"
        ));
    }

    let root = workspace_root()?;
    let candidate = resolve_path_candidate(raw, &root);
    let mut anchor = candidate
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.clone());
    while !anchor.exists() {
        let Some(next) = anchor.parent().map(Path::to_path_buf) else {
            return Err(anyhow!("无法定位可写目录：{}", candidate.display()));
        };
        anchor = next;
    }
    let parent_canonical = anchor
        .canonicalize()
        .with_context(|| format!("解析目标目录失败：{}", anchor.display()))?;
    let roots = allowed_roots()?;

    if !is_path_in_allowed_roots(&parent_canonical, &roots) {
        return Err(anyhow!(
            "路径越界，仅允许当前目录或临时目录：{}",
            candidate.display()
        ));
    }
    Ok(candidate)
}

fn resolve_path_candidate(raw: &str, workspace: &Path) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

fn is_path_in_allowed_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

pub(super) fn validate_command_args_in_allowed_roots(cmd: &str, args: &[String]) -> Result<()> {
    if matches!(cmd, "echo" | "pwd") {
        return Ok(());
    }

    let workspace = workspace_root()?;
    let roots = allowed_roots()?;
    let mut skip_next_value = false;
    for arg in args {
        let raw = arg.trim();
        if raw.is_empty() {
            continue;
        }
        if skip_next_value {
            skip_next_value = false;
            continue;
        }
        if raw.starts_with('-') {
            skip_next_value = option_requires_value(cmd, raw);
            continue;
        }
        if !argument_may_be_path(cmd, raw) {
            continue;
        }
        ensure_command_arg_path_allowed(raw, &workspace, &roots)?;
    }
    Ok(())
}

fn option_requires_value(cmd: &str, option: &str) -> bool {
    match cmd {
        "head" | "tail" => matches!(option, "-n" | "--lines" | "-c" | "--bytes"),
        _ => false,
    }
}

fn argument_may_be_path(cmd: &str, raw: &str) -> bool {
    match cmd {
        "ls" | "cat" | "wc" => true,
        "head" | "tail" => !raw
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '+' || ch == '-'),
        _ => false,
    }
}

fn ensure_command_arg_path_allowed(raw: &str, workspace: &Path, roots: &[PathBuf]) -> Result<()> {
    if raw.starts_with('~') {
        return Err(anyhow!(
            "命令参数路径不允许使用 home 目录：{raw}；仅允许当前目录与临时目录"
        ));
    }

    let candidate = resolve_path_candidate(raw, workspace);
    let anchor = if candidate.exists() {
        candidate
    } else {
        let mut parent = candidate
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("命令参数路径非法，无法解析父目录：{}", candidate.display()))?;
        while !parent.exists() {
            parent = parent.parent().map(Path::to_path_buf).ok_or_else(|| {
                anyhow!("命令参数路径非法，无法解析父目录：{}", candidate.display())
            })?;
        }
        parent
    };

    let canonical = anchor
        .canonicalize()
        .with_context(|| format!("解析命令参数路径失败：{}", anchor.display()))?;
    if !is_path_in_allowed_roots(&canonical, roots) {
        return Err(anyhow!(
            "命令参数路径越界，仅允许当前目录或临时目录：{}",
            raw
        ));
    }
    Ok(())
}

pub(super) fn display_rel_path(path: &Path) -> String {
    let root = match workspace_root().and_then(|root| {
        root.canonicalize()
            .with_context(|| format!("解析工作目录失败：{}", root.display()))
    }) {
        Ok(root) => root,
        Err(_) => return path.display().to_string(),
    };

    path.strip_prefix(&root)
        .map(|rel| {
            let rel_text = rel.display().to_string();
            if rel_text.is_empty() {
                ".".to_string()
            } else {
                rel_text
            }
        })
        .unwrap_or_else(|_| path.display().to_string())
}

pub(super) fn is_allowed_command(cmd: &str) -> bool {
    matches!(cmd, "echo" | "pwd" | "ls" | "cat" | "head" | "tail" | "wc")
}

pub(super) fn validate_bash_args(args: &[String]) -> Result<()> {
    if args.len() != 2 || args.first().map(String::as_str) != Some("-lc") {
        return Err(anyhow!(
            "bash 仅允许以 -lc 单脚本形式执行：run_command(cmd=bash,args=[\"-lc\",\"<script>\"])"
        ));
    }
    let script = args
        .get(1)
        .map(String::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if script.is_empty() {
        return Err(anyhow!("bash 脚本不能为空"));
    }
    validate_bash_script(script)
}

fn validate_bash_script(script: &str) -> Result<()> {
    let lowered = script.to_ascii_lowercase();
    if contains_forbidden_bash_tokens(&lowered) {
        return Err(anyhow!("bash 脚本包含不允许的高风险控制符或命令"));
    }

    let cmd = extract_bash_head_command(script).ok_or_else(|| anyhow!("无法识别 bash 命令"))?;
    if !is_allowed_bash_head_command(cmd) {
        return Err(anyhow!("bash 脚本首命令不在允许列表：{cmd}"));
    }

    let args = script
        .split_whitespace()
        .skip(1)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    validate_command_args_in_allowed_roots(cmd, &args)
}

fn contains_forbidden_bash_tokens(script: &str) -> bool {
    const FORBIDDEN: [&str; 17] = [
        "&&", "||", ";", "|", ">", "<", "`", "$(", "sudo ", " rm -", "mv /", "chmod -r", "chown ",
        "shutdown", "reboot", "poweroff", "mkfs",
    ];
    FORBIDDEN.iter().any(|token| script.contains(token))
}

fn extract_bash_head_command(script: &str) -> Option<&str> {
    script.split_whitespace().next()
}

fn is_allowed_bash_head_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "echo" | "pwd" | "ls" | "cat" | "head" | "tail" | "wc" | "rg" | "grep" | "cargo" | "git"
    )
}

pub(super) fn command_timeout_ms() -> u64 {
    const DEFAULT_TIMEOUT_MS: u64 = 10_000;
    std::env::var("TOOL_COMMAND_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
}

pub(super) fn execute_command_with_timeout(
    command: &mut Command,
    timeout_ms: u64,
) -> Result<(Output, bool)> {
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

pub(super) fn elapsed_ms_u64(raw: u128) -> u64 {
    raw.min(u64::MAX as u128) as u64
}

pub(super) fn truncate_output(raw: &str) -> String {
    const MAX_CHARS: usize = 6000;
    let mut output = raw.chars().take(MAX_CHARS).collect::<String>();
    if raw.chars().count() > MAX_CHARS {
        output.push_str("\n...(truncated)");
    }
    output
}
