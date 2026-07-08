//! 工具共享 helper：会话工作目录、路径沙箱、命令白名单、命令执行。
//!
//! 原 `tiangong-core::tool::common`，随收敛重构迁出为独立 crate（#208）。
//! 作为路径沙箱安全基础设施，供 core 与各进程内插件 crate（fs / command / fetch /
//! index / terminal）共用，避免重复实现安全逻辑。
//!
//! 路径解析相关 helper 提供两套 API：
//! - 隐式 thread-local 版本（`resolve_workspace_path` 等）：依赖 `SESSION_CWD`，
//!   供 core 沿用旧行为；
//! - 显式注入版本（`*_with_base`）：接收 `base: &Path` 参数，供插件 handler 使用，
//!   无需关心调用方是否设置了 thread-local CWD。

use std::cell::RefCell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

use tiangong_types::process::configure_no_window;

thread_local! {
    /// 当前执行的会话工作目录，由 RuntimeEngine 在执行前设置
    static SESSION_CWD: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// 设置当前线程的会话工作目录
pub fn set_session_cwd(cwd: Option<PathBuf>) {
    SESSION_CWD.with(|cell| *cell.borrow_mut() = cwd);
}

pub fn workspace_root() -> Result<PathBuf> {
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

/// 读取当前线程的会话工作目录，供需要跟随会话 workspace 的子系统（如 stdio MCP 子进程）使用。
///
/// 仅当线程已通过 [`set_session_cwd`] 设置过有效目录时返回 `Some`；否则返回 `None`，
/// 由调用方自行决定回退策略（典型做法是不设置 `current_dir`，让子进程继承宿主 cwd）。
pub fn session_workspace_root() -> Option<PathBuf> {
    SESSION_CWD.with(|cell| cell.borrow().as_ref().filter(|cwd| cwd.is_dir()).cloned())
}

/// 插件贡献的额外允许文件根目录（process-level，由 core/mod.rs 汇总各插件的
/// `Plugin::allowed_file_roots` 写入）。
static EXTRA_ALLOWED_ROOTS: OnceLock<RwLock<Vec<PathBuf>>> = OnceLock::new();

fn extra_allowed_roots() -> &'static RwLock<Vec<PathBuf>> {
    EXTRA_ALLOWED_ROOTS.get_or_init(|| RwLock::new(Vec::new()))
}

/// 写入插件贡献的额外允许文件根目录（供 core/mod.rs 在汇总插件能力时调用）。
pub fn set_extra_allowed_roots(roots: Vec<PathBuf>) {
    if let Ok(mut guard) = extra_allowed_roots().write() {
        *guard = roots;
    }
}

/// 计算允许写入的根目录列表（工作空间 + 插件贡献的额外根目录）。
///
/// 显式传入 `workspace`，供无 thread-local CWD 的插件 handler 调用，
/// 避免隐式依赖 `SESSION_CWD`。额外根目录由各插件经
/// `Plugin::allowed_file_roots` 贡献，由 core 汇总后通过 `set_extra_allowed_roots` 注入。
fn write_allowed_roots_with(workspace: &Path) -> Result<Vec<PathBuf>> {
    let workspace_canonical = workspace
        .canonicalize()
        .with_context(|| format!("解析工作目录失败：{}", workspace.display()))?;

    let mut roots = vec![workspace_canonical];
    // 插件贡献的额外允许目录（如 skill plugin 的 ~/.tiangong/skills）。
    if let Ok(extra) = extra_allowed_roots().read() {
        for root in extra.iter() {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
            if !roots.iter().any(|r| r == &canonical) {
                roots.push(canonical);
            }
        }
    }
    Ok(roots)
}

pub fn write_allowed_roots() -> Result<Vec<PathBuf>> {
    let workspace = workspace_root()?;
    write_allowed_roots_with(&workspace)
}

/// 基于 `base` 工作目录校验路径是否落在允许写入的根目录内。
fn ensure_path_in_write_allowed_roots_with(path: &Path, label: &str, base: &Path) -> Result<()> {
    let roots = write_allowed_roots_with(base)?;
    let canonical = path
        .canonicalize()
        .with_context(|| format!("解析{label}失败：{}", path.display()))?;
    if !is_path_in_allowed_roots(&canonical, &roots) {
        return Err(anyhow!(
            "{label}越界，仅允许当前工作空间或已注册的额外允许目录：{}",
            canonical.display()
        ));
    }
    Ok(())
}

/// 解析 effective cwd（显式传入工作目录，供插件 handler 使用）。
pub fn resolve_effective_cwd_with(raw: Option<&str>, base: &Path) -> Result<PathBuf> {
    let value = raw.unwrap_or(".").trim();
    let cwd = if value.is_empty() {
        base.canonicalize()
            .with_context(|| format!("解析工作目录失败：{}", base.display()))?
    } else {
        resolve_path_from_base(value, base)?
    };
    if !cwd.is_dir() {
        return Err(anyhow!("workdir 不是目录：{}", cwd.display()));
    }
    ensure_path_in_write_allowed_roots_with(&cwd, "workdir", base)?;
    Ok(cwd)
}

/// 解析工作空间内的读路径（显式传入工作目录，供插件 handler 使用）。
pub fn resolve_workspace_path_with(raw: &str, base: &Path) -> Result<PathBuf> {
    resolve_path_from_base(raw, base)
}

/// 信任模式下的路径解析（显式传入工作目录）：不做越界检查，路径不存在时不报错。
pub fn resolve_workspace_path_trusted_with(raw: &str, base: &Path) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }
    let candidate = resolve_path_candidate(raw, base);
    // 存在时 canonicalize 解析符号链接，不存在时直接返回
    Ok(candidate.canonicalize().unwrap_or(candidate))
}

pub fn resolve_path_from_base(raw: &str, base: &Path) -> Result<PathBuf> {
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

pub fn resolve_write_path_from_base(raw: &str, base: &Path) -> Result<PathBuf> {
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
    let roots = write_allowed_roots_with(base)?;

    if !is_path_in_allowed_roots(&parent_canonical, &roots) {
        return Err(anyhow!(
            "路径越界，仅允许当前工作空间或已注册的额外允许目录：{}",
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
            "{label}越界，仅允许当前工作空间或已注册的额外允许目录：{}",
            canonical.display()
        ));
    }
    Ok(())
}

pub fn validate_command_args_in_allowed_roots(
    cmd: &str,
    args: &[String],
    base_dir: &Path,
) -> Result<()> {
    if matches!(cmd, "echo" | "pwd") || !command_may_write_files(cmd) {
        return Ok(());
    }

    let roots = write_allowed_roots_with(base_dir)?;
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

/// 相对工作目录的路径展示（显式传入工作目录，供插件 handler 使用）。
pub fn display_rel_path_with(path: &Path, base: &Path) -> String {
    let root = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
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

pub fn is_allowed_command(cmd: &str) -> bool {
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

/// 命令校验结果。
///
/// 白名单机制不做硬性拦截——白名单外的命令不再直接拒绝，而是返回
/// [`CommandValidation::NeedsApproval`]，由上层 PermissionGate 审批网关决定
/// 是否放行（Supervised 模式下走用户审批）。硬性拒绝（forbidden tokens、
/// 路径越界、shell 形式不合法）仍通过 `Err` 返回。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandValidation {
    /// 命令在内置白名单或用户扩展白名单内，校验通过。
    Allowed,
    /// 命令不在白名单内，但未命中硬性拒绝条件，需走审批流程。
    NeedsApproval { cmd: String },
}

/// 判断命令是否在允许列表内（内置白名单 + 用户扩展）。
pub fn is_command_allowed(cmd: &str, extra_allowed: &[String]) -> bool {
    is_allowed_command(cmd) || extra_allowed.iter().any(|c| c == cmd)
}

/// 校验 shell 脚本命令（`bash -lc`/`sh -c`/`powershell -Command` 形式）。
///
/// 返回 [`CommandValidation`] 表示白名单校验结果；`Err` 仅用于硬性拒绝
///（forbidden tokens、重定向、shell 形式不合法、路径越界）。
pub fn validate_shell_command_args(
    shell_cmd: &str,
    args: &[String],
    base_dir: &Path,
    extra_allowed: &[String],
) -> Result<CommandValidation> {
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
    validate_shell_script(script, base_dir, extra_allowed)
}

fn script_contains_write_redirection(script: &str) -> bool {
    script.contains(">>") || script.contains('>') || script.contains("<<")
}

fn validate_shell_script(
    script: &str,
    base_dir: &Path,
    extra_allowed: &[String],
) -> Result<CommandValidation> {
    let lowered = script.to_ascii_lowercase();
    if contains_forbidden_shell_tokens(&lowered) {
        return Err(anyhow!("shell 脚本包含不允许的高风险控制符或命令"));
    }

    // 按 &&、||、; 分割为子命令，逐个验证
    let sub_commands = split_shell_commands(script);
    let mut needs_approval_cmd: Option<String> = None;
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

        // 白名单（内置 + 用户扩展）内的命令直接通过；白名单外的不报错，
        // 记录下需要审批的命令名，最终返回 NeedsApproval。
        if !is_shell_head_allowed(cmd, extra_allowed) {
            needs_approval_cmd = Some(cmd.to_string());
            continue;
        }

        // 检查路径参数是否在允许范围内
        let args: Vec<String> = sub
            .split_whitespace()
            .skip(1)
            .map(ToString::to_string)
            .collect();
        validate_command_args_in_allowed_roots(cmd, &args, base_dir)?;
    }
    Ok(match needs_approval_cmd {
        Some(cmd) => CommandValidation::NeedsApproval { cmd },
        None => CommandValidation::Allowed,
    })
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

/// 判断 shell 脚本首命令是否在允许列表内（内置 shell head 白名单 + 用户扩展）。
///
/// shell head 白名单比 [`is_allowed_command`] 多了控制流关键字（`for`/`while`/`if`）
/// 和 shell 内建（`cd`/`test`/`[`/`nohup`/`screen`/`tmux`/`tar`/`unzip`）。
fn is_shell_head_allowed(cmd: &str, extra_allowed: &[String]) -> bool {
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
        || extra_allowed.iter().any(|c| c == cmd)
}

pub fn derive_shell_exec_args(script: &str, shell: Option<&str>) -> Result<(String, Vec<String>)> {
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

pub fn command_env_allowlist() -> Vec<(String, String)> {
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

pub fn command_timeout_ms() -> u64 {
    // 默认 30 秒超时，防止工具执行卡住
    // 可通过 TOOL_COMMAND_TIMEOUT_MS 环境变量覆盖（毫秒，0 表示无限等待）
    std::env::var("TOOL_COMMAND_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(30_000)
}

pub fn execute_command_with_timeout(
    command: &mut Command,
    timeout_ms: u64,
) -> Result<(Output, bool)> {
    configure_no_window(command);

    // timeout_ms=0 表示不设超时：标准库 output() 会内部等待并收集输出，
    // 不走当前自定义轮询逻辑，也不会出现 pipe 堵塞问题。
    if timeout_ms == 0 {
        let output = command.output().context("执行命令失败")?;
        return Ok((output, false));
    }

    // 显式 piped：用独立线程实时 drain stdout/stderr，避免子进程输出写满 pipe 缓冲区
    //（通常 64KB）后阻塞在写端，导致父进程 try_wait 永远等不到退出、最终被超时杀。
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command.spawn().context("spawn 子进程失败")?;

    // 取出 stdout/stderr 句柄，各自起线程持续读取到独立 buffer。
    // 用 Box<dyn Read> 统一 ChildStdout / ChildStdErr 两种类型，复用同一个 drain 函数。
    let stdout: Option<Box<dyn Read + Send>> = child
        .stdout
        .take()
        .map(|s| Box::new(s) as Box<dyn Read + Send>);
    let stderr: Option<Box<dyn Read + Send>> = child
        .stderr
        .take()
        .map(|s| Box::new(s) as Box<dyn Read + Send>);
    let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));

    let stdout_handle = spawn_drain(stdout, stdout_buf.clone());
    let stderr_handle = spawn_drain(stderr, stderr_buf.clone());

    let timeout = Duration::from_millis(timeout_ms);
    let started = Instant::now();
    let mut timed_out = false;

    let status = loop {
        if let Some(status) = child.try_wait().context("轮询子进程状态失败")? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            timed_out = true;
            // kill 后再 wait 拿到最终 status（try_wait 可能需要一次循环才返回）。
            let status = child.wait().context("等待被杀子进程退出失败")?;
            break status;
        }
        thread::sleep(Duration::from_millis(20));
    };

    // 等待两个 drain 线程结束（子进程退出或被 kill 后 pipe 关闭，读线程收到 EOF 返回）。
    // join 失败（读线程意外 panic）时用已收集的部分输出兜底。
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    let stdout = take_buf(&stdout_buf);
    let stderr = take_buf(&stderr_buf);

    Ok((
        Output {
            status,
            stdout,
            stderr,
        },
        timed_out,
    ))
}

/// 单路输出最多收集的字节数。达到上限后继续 drain（避免 pipe 再堵）但丢弃后续内容，
/// 防止命中极多结果（如搜索 `use`/`fn`）时把内存吃满。
/// 上限刻意大于 `truncate_output` 的 6000 字符，给调用方留出截断判断的余量。
const MAX_CAPTURE_BYTES: usize = 512 * 1024;

/// 起一个线程持续读取 `pipe` 到共享 buffer，直到 EOF 或达到收集上限。
///
/// 达到 `MAX_CAPTURE_BYTES` 后线程**继续读取但不再写入 buffer**——这样既避免 pipe
/// 缓冲区再次被写满导致子进程阻塞，又保证收集的输出有内存上限。调用方应结合
/// `truncate_output` 在结果进入 LLM 上下文前做二次截断。
fn spawn_drain(
    pipe: Option<Box<dyn Read + Send>>,
    buf: Arc<Mutex<Vec<u8>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut tmp = [0u8; 8192];
        loop {
            match pipe.read(&mut tmp) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    // 达到上限后只读不存：保持 pipe 畅通，但不再增长 buffer。
                    // 先无副作用判满（避免持锁状态下 pipe 关闭导致死锁），未满再写入。
                    let already_full = buf
                        .lock()
                        .map(|g| g.len() >= MAX_CAPTURE_BYTES)
                        .unwrap_or(false);
                    if !already_full && let Ok(mut guard) = buf.lock() {
                        let remaining = MAX_CAPTURE_BYTES.saturating_sub(guard.len());
                        let take = n.min(remaining);
                        guard.extend_from_slice(&tmp[..take]);
                    }
                }
                Err(_) => break, // 读取错误（含 pipe 被关闭），结束线程
            }
        }
    })
}

/// 从共享 buffer 取出全部字节（清空原 buffer，避免长期持锁）。
fn take_buf(buf: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    buf.lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

pub fn truncate_output(raw: &str) -> String {
    const MAX_CHARS: usize = 6000;
    let mut output = raw.chars().take(MAX_CHARS).collect::<String>();
    if raw.chars().count() > MAX_CHARS {
        output.push_str("\n...(truncated)");
    }
    output
}
