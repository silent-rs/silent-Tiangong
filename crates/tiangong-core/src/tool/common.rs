use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

thread_local! {
    /// 当前执行的会话工作目录，由 RuntimeEngine 在执行前设置
    static SESSION_CWD: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// 设置当前线程的会话工作目录
pub fn set_session_cwd(cwd: Option<PathBuf>) {
    SESSION_CWD.with(|cell| *cell.borrow_mut() = cwd);
}

pub(super) fn workspace_root() -> Result<PathBuf> {
    // 优先使用会话级工作目录，回退到进程工作目录
    SESSION_CWD.with(|cell| {
        if let Some(ref cwd) = *cell.borrow()
            && cwd.is_dir()
        {
            return Ok(cwd.clone());
        }
        std::env::current_dir().context("读取当前工作目录失败")
    })
}

fn write_allowed_roots() -> Result<Vec<PathBuf>> {
    let workspace = workspace_root()?;
    let workspace_canonical = workspace
        .canonicalize()
        .with_context(|| format!("解析工作目录失败：{}", workspace.display()))?;

    let mut roots = vec![workspace_canonical];
    if let Some(home) = user_home_dir() {
        let skills = home.join(".tiangong").join("skills");
        let skills_canonical = skills.canonicalize().unwrap_or(skills);
        if !roots.iter().any(|root| root == &skills_canonical) {
            roots.push(skills_canonical);
        }
    }
    Ok(roots)
}

pub(super) fn resolve_effective_cwd(raw: Option<&str>) -> Result<PathBuf> {
    let root = workspace_root()?;
    let value = raw.unwrap_or(".").trim();
    let cwd = if value.is_empty() {
        root.canonicalize()
            .with_context(|| format!("解析工作目录失败：{}", root.display()))?
    } else {
        resolve_path_from_base(value, &root)?
    };
    if !cwd.is_dir() {
        return Err(anyhow!("workdir 不是目录：{}", cwd.display()));
    }
    ensure_path_in_write_allowed_roots(&cwd, "workdir")?;
    Ok(cwd)
}

pub(super) fn resolve_workspace_path(raw: &str) -> Result<PathBuf> {
    let base = workspace_root()?;
    resolve_path_from_base(raw, &base)
}

/// 信任模式下的路径解析：不做越界检查，路径不存在时不报错
pub(super) fn resolve_workspace_path_trusted(raw: &str) -> Result<PathBuf> {
    let base = workspace_root()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }
    let candidate = resolve_path_candidate(raw, &base);
    // 存在时 canonicalize 解析符号链接，不存在时直接返回
    Ok(candidate.canonicalize().unwrap_or(candidate))
}

pub(super) fn resolve_workspace_write_path(raw: &str) -> Result<PathBuf> {
    let base = workspace_root()?;
    resolve_write_path_from_base(raw, &base)
}

/// 信任模式下仍然限制写入范围，仅放宽目标文件不存在的场景。
pub(super) fn resolve_workspace_write_path_trusted(raw: &str) -> Result<PathBuf> {
    let base = workspace_root()?;
    resolve_write_path_from_base(raw, &base)
}

pub(super) fn resolve_path_from_base(raw: &str, base: &Path) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }

    let candidate = resolve_path_candidate(raw, base);
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("解析路径失败：{}", candidate.display()))?;
    Ok(canonical)
}

pub(super) fn resolve_write_path_from_base(raw: &str, base: &Path) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }

    let candidate = resolve_path_candidate(raw, base);
    let mut anchor = candidate
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| base.to_path_buf());
    while !anchor.exists() {
        let Some(next) = anchor.parent().map(Path::to_path_buf) else {
            return Err(anyhow!("无法定位可写目录：{}", candidate.display()));
        };
        anchor = next;
    }
    let parent_canonical = anchor
        .canonicalize()
        .with_context(|| format!("解析目标目录失败：{}", anchor.display()))?;
    let roots = write_allowed_roots()?;

    if !is_path_in_allowed_roots(&parent_canonical, &roots) {
        return Err(anyhow!(
            "路径越界，仅允许当前工作空间或 ~/.tiangong/skills：{}",
            candidate.display()
        ));
    }
    Ok(candidate)
}

fn resolve_path_candidate(raw: &str, base: &Path) -> PathBuf {
    if let Some(expanded) = expand_home_path(raw) {
        return expanded;
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn is_path_in_allowed_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn ensure_path_in_write_allowed_roots(path: &Path, label: &str) -> Result<()> {
    let roots = write_allowed_roots()?;
    let canonical = path
        .canonicalize()
        .with_context(|| format!("解析{label}失败：{}", path.display()))?;
    if !is_path_in_allowed_roots(&canonical, &roots) {
        return Err(anyhow!(
            "{label}越界，仅允许当前工作空间或 ~/.tiangong/skills：{}",
            canonical.display()
        ));
    }
    Ok(())
}

pub(super) fn validate_command_args_in_allowed_roots(
    cmd: &str,
    args: &[String],
    base_dir: &Path,
) -> Result<()> {
    if matches!(cmd, "echo" | "pwd") || !command_may_write_files(cmd) {
        return Ok(());
    }

    let roots = write_allowed_roots()?;
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
        if !write_command_argument_may_be_path(cmd, raw) {
            continue;
        }
        ensure_command_arg_path_allowed(raw, base_dir, &roots)?;
    }
    Ok(())
}

fn option_requires_value(cmd: &str, option: &str) -> bool {
    match cmd {
        "head" | "tail" => matches!(option, "-n" | "--lines" | "-c" | "--bytes"),
        _ => false,
    }
}

fn command_may_write_files(cmd: &str) -> bool {
    matches!(
        cmd,
        "cp" | "mv"
            | "rm"
            | "mkdir"
            | "touch"
            | "chmod"
            | "ln"
            | "cargo"
            | "git"
            | "node"
            | "npm"
            | "npx"
            | "yarn"
            | "pnpm"
            | "ts-node"
            | "python"
            | "python3"
            | "pip"
            | "pip3"
            | "pipx"
            | "uv"
            | "uvx"
            | "sea-orm-cli"
            | "bash"
            | "sh"
            | "powershell"
            | "pwsh"
    )
}

fn write_command_argument_may_be_path(cmd: &str, raw: &str) -> bool {
    match cmd {
        "cp" | "mv" | "rm" | "mkdir" | "touch" | "chmod" | "ln" => true,
        "cargo" | "git" | "node" | "npm" | "npx" | "yarn" | "pnpm" | "ts-node" | "python"
        | "python3" | "pip" | "pip3" | "pipx" | "uv" | "uvx" | "sea-orm-cli" => {
            raw.contains('/') || raw == "." || raw == ".." || raw.starts_with('~')
        }
        _ => false,
    }
}

fn ensure_command_arg_path_allowed(raw: &str, base_dir: &Path, roots: &[PathBuf]) -> Result<()> {
    let candidate = resolve_path_candidate(raw, base_dir);
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
            "命令参数路径越界，仅允许当前目录、~/.tiangong 或临时目录：{}",
            raw
        ));
    }
    Ok(())
}

fn expand_home_path(raw: &str) -> Option<PathBuf> {
    if raw == "~" {
        return user_home_dir();
    }
    if let Some(suffix) = raw.strip_prefix("~/") {
        return user_home_dir().map(|home| home.join(suffix));
    }
    None
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = PathBuf::from(drive);
            buf.push(path);
            Some(buf)
        }
        _ => None,
    }
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
    matches!(
        cmd,
        // 基础命令
        "echo"
            | "pwd"
            | "ls"
            | "cat"
            | "head"
            | "tail"
            | "wc"
            | "rg"
            | "grep"
            | "find"
            | "which"
            | "env"
            | "printenv"
            // 文件操作
            | "cp"
            | "mv"
            | "rm"
            | "mkdir"
            | "touch"
            | "chmod"
            | "ln"
            // 开发工具
            | "cargo"
            | "git"
            | "node"
            | "npm"
            | "npx"
            | "yarn"
            | "pnpm"
            | "ts-node"
            | "python"
            | "python3"
            | "pip"
            | "pip3"
            | "pipx"
            | "uv"
            | "uvx"
            | "sea-orm-cli"
            // 网络工具
            | "curl"
            | "wget"
            // Shell
            | "bash"
            | "sh"
            | "powershell"
            | "pwsh"
    )
}

pub(super) fn validate_shell_command_args(
    shell_cmd: &str,
    args: &[String],
    base_dir: &Path,
) -> Result<()> {
    let (expected_flag, flag_label) = match shell_cmd {
        "bash" => ("-lc", "bash -lc"),
        "sh" => ("-c", "sh -c"),
        "powershell" | "pwsh" => ("-Command", "powershell -Command"),
        _ => return Err(anyhow!("不支持的 shell 命令：{shell_cmd}")),
    };
    if args.len() != 2 || args.first().map(String::as_str) != Some(expected_flag) {
        return Err(anyhow!(
            "{shell_cmd} 仅允许 {flag_label} 单脚本形式：run_shell(script=...) 或 run_command(cmd={shell_cmd},args=[\"{expected_flag}\",\"<script>\"])"
        ));
    }
    let script = args
        .get(1)
        .map(String::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if script.is_empty() {
        return Err(anyhow!("{shell_cmd} 脚本不能为空"));
    }
    if script_contains_write_redirection(script) {
        return Err(anyhow!(
            "shell 脚本不允许使用重定向写入，请改用受控文件工具"
        ));
    }
    validate_shell_script(script, base_dir)
}

fn script_contains_write_redirection(script: &str) -> bool {
    script.contains(">>") || script.contains('>') || script.contains("<<")
}

fn validate_shell_script(script: &str, base_dir: &Path) -> Result<()> {
    let lowered = script.to_ascii_lowercase();
    if contains_forbidden_shell_tokens(&lowered) {
        return Err(anyhow!("shell 脚本包含不允许的高风险控制符或命令"));
    }

    // 按 &&、||、; 分割为子命令，逐个验证
    let sub_commands = split_shell_commands(script);
    for sub in &sub_commands {
        let sub = sub.trim();
        if sub.is_empty() {
            continue;
        }
        // 跳过注释行和 shebang
        if sub.starts_with('#') {
            continue;
        }
        // 跳过管道后面的部分
        let sub = sub.split('|').next().unwrap_or(sub).trim();
        if sub.is_empty() {
            continue;
        }
        // 去掉末尾的后台符 &
        let sub = sub.trim_end_matches('&').trim();
        if sub.is_empty() {
            continue;
        }

        let cmd = extract_shell_head_command(sub)
            .ok_or_else(|| anyhow!("无法识别 shell 脚本命令：{sub}"))?;

        // cd 不需要命令白名单检查，但路径需要在允许范围内
        if cmd == "cd" {
            let target = sub.split_whitespace().nth(1).unwrap_or(".");
            let resolved = resolve_path_candidate(target, base_dir);
            ensure_path_in_write_allowed_roots(&resolved, "shell 脚本 cd 目标")?;
            continue;
        }

        if !is_allowed_shell_head_command(cmd) {
            return Err(anyhow!("shell 脚本命令不在允许列表：{cmd}"));
        }

        // 检查路径参数是否在允许范围内
        let args: Vec<String> = sub
            .split_whitespace()
            .skip(1)
            .map(ToString::to_string)
            .collect();
        validate_command_args_in_allowed_roots(cmd, &args, base_dir)?;
    }
    Ok(())
}

/// 按 &&、||、;、换行 分割 shell 命令
fn split_shell_commands(script: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut current = String::new();
    let mut chars = script.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                current.push(ch);
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                current.push(ch);
            }
            '&' if !in_single_quote && !in_double_quote && chars.peek() == Some(&'&') => {
                chars.next(); // consume second &
                commands.push(current.clone());
                current.clear();
            }
            '|' if !in_single_quote && !in_double_quote && chars.peek() == Some(&'|') => {
                chars.next(); // consume second |
                commands.push(current.clone());
                current.clear();
            }
            ';' | '\n' if !in_single_quote && !in_double_quote => {
                commands.push(current.clone());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        commands.push(current);
    }
    commands
}

fn contains_forbidden_shell_tokens(script: &str) -> bool {
    const FORBIDDEN: [&str; 8] = [
        "sudo ", "chmod -r", "chown ", "shutdown", "reboot", "poweroff", "mkfs", "dd if=",
    ];
    FORBIDDEN.iter().any(|token| script.contains(token))
}

fn extract_shell_head_command(script: &str) -> Option<&str> {
    script.split_whitespace().next()
}

fn is_allowed_shell_head_command(cmd: &str) -> bool {
    // shell 脚本首命令白名单（与 is_allowed_command 保持一致）
    is_allowed_command(cmd)
        || matches!(
            cmd,
            "cd" | "curl"
                | "wget"
                | "tar"
                | "unzip"
                | "test"
                | "["
                | "nohup"
                | "screen"
                | "tmux"
                | "for"
                | "while"
                | "if"
        )
}

pub(super) fn derive_shell_exec_args(
    script: &str,
    shell: Option<&str>,
) -> Result<(String, Vec<String>)> {
    let shell = shell
        .unwrap_or("auto")
        .trim()
        .to_ascii_lowercase()
        .replace(' ', "");
    let selected = match shell.as_str() {
        "" | "auto" => {
            if cfg!(target_os = "windows") {
                "powershell"
            } else {
                "bash"
            }
        }
        "bash" => "bash",
        "sh" => "sh",
        "powershell" => "powershell",
        "pwsh" => "pwsh",
        other => return Err(anyhow!("不支持的 shell 类型：{other}")),
    };

    let args = match selected {
        "bash" => vec!["-lc".to_string(), script.to_string()],
        "sh" => vec!["-c".to_string(), script.to_string()],
        "powershell" | "pwsh" => vec!["-Command".to_string(), script.to_string()],
        _ => return Err(anyhow!("不支持的 shell 类型：{selected}")),
    };
    Ok((selected.to_string(), args))
}

pub(super) fn command_env_allowlist() -> Vec<(String, String)> {
    const ALLOWED: [&str; 21] = [
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
        "TERM",
        "SystemRoot",
        "ComSpec",
        "PATHEXT",
        // 代理设置
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ];
    ALLOWED
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_string(), value))
        })
        .collect::<Vec<_>>()
}

pub(super) fn command_timeout_ms() -> u64 {
    // 默认 30 秒超时，防止工具执行卡住
    // 可通过 TOOL_COMMAND_TIMEOUT_MS 环境变量覆盖（毫秒，0 表示无限等待）
    std::env::var("TOOL_COMMAND_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(30_000)
}

pub(super) fn execute_command_with_timeout(
    command: &mut Command,
    timeout_ms: u64,
) -> Result<(Output, bool)> {
    let mut child = command.spawn().context("spawn 子进程失败")?;

    // timeout_ms=0 表示不设超时
    if timeout_ms == 0 {
        let output = child.wait_with_output().context("读取命令输出失败")?;
        return Ok((output, false));
    }

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
