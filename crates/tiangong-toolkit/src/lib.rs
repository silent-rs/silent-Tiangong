//! 工具共享 helper：会话工作目录、路径沙箱、命令白名单、命令执行。
//!
//! 原 `tiangong-core::tool::common`，随收敛重构迁出为独立 crate（#208）。
//! 作为路径沙箱安全基础设施，供 core 与各进程内插件 crate（fs / command / fetch /
//! index / terminal）共用，避免重复实现安全逻辑。
//!
//! 会话工作目录的注入采用显式传递：core 在 `prepare_plugins` 阶段把 `Session.cwd`
//! 经各插件的 `set_workspace` 注入；stdio MCP 子进程等需要 cwd 的执行路径由
//! 插件沿调用链显式携带（见 `LocalMcpClient.workspace`），不依赖 thread-local。
//! [`workspace_root`] 仅作无显式 base 时的回退，读进程 cwd。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
mod path;
use path::canonicalize_path;

pub mod process;

pub use process::{configure_no_window, configure_tokio_no_window};

/// 读取进程当前工作目录，供无显式 base 注入的回退路径使用。
///
/// 历史上这里曾优先读取 thread-local `SESSION_CWD`，但该机制在 Core 重构
/// （`fe5b026c`）后不再有注入方；会话工作目录改由插件显式注入，此函数仅作
/// 进程 cwd 兜底。
pub fn workspace_root() -> Result<PathBuf> {
    std::env::current_dir().context("读取当前工作目录失败")
}

/// 应用自管存储根目录：`~/.tiangong/`。
///
/// 该目录承载 skills / MCP 锁等插件自管数据，天然属于应用可信存储区，
/// 始终允许 fs 工具写入（如 Agent 创建/编辑 skill 文件），无需经插件 hook 声明。
pub fn app_storage_root() -> PathBuf {
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
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

/// 计算允许写入的根目录列表（工作空间 + 应用存储根）。
///
/// 显式传入 `workspace`，供无 thread-local CWD 的插件 handler 调用，
/// 避免隐式依赖 `SESSION_CWD`。应用存储根（`~/.tiangong/`）天然可信，
/// 始终追加，无需插件经 hook 声明。
fn write_allowed_roots_with(workspace: &Path) -> Result<Vec<PathBuf>> {
    let workspace_canonical = canonicalize_path(workspace)
        .with_context(|| format!("解析工作目录失败：{}", workspace.display()))?;

    let mut roots = vec![workspace_canonical];
    // 应用自管存储根（~/.tiangong/），始终允许。
    let storage = app_storage_root();
    let storage_canonical = canonicalize_path(&storage).unwrap_or(storage);
    if !roots.iter().any(|r| r == &storage_canonical) {
        roots.push(storage_canonical);
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
    let canonical =
        canonicalize_path(path).with_context(|| format!("解析{label}失败：{}", path.display()))?;
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
        canonicalize_path(base).with_context(|| format!("解析工作目录失败：{}", base.display()))?
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
    Ok(canonicalize_path(&candidate).unwrap_or(candidate))
}

pub fn resolve_path_from_base(raw: &str, base: &Path) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }

    let candidate = resolve_path_candidate(raw, base);
    let canonical = canonicalize_path(&candidate)
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
    let parent_canonical = canonicalize_path(&anchor)
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
    let root = canonicalize_path(base).unwrap_or_else(|_| base.to_path_buf());
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
