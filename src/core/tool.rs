use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolName {
    ReadFile,
    ListDir,
    RunCommand,
    SearchCode,
    WriteFile,
    ReplaceInFile,
    ApplyPatch,
}

impl ToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::ListDir => "list_dir",
            Self::RunCommand => "run_command",
            Self::SearchCode => "search_code",
            Self::WriteFile => "write_file",
            Self::ReplaceInFile => "replace_in_file",
            Self::ApplyPatch => "apply_patch",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: ToolName,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionRecord {
    pub tool_name: String,
    pub args: Vec<String>,
    pub duration_ms: u64,
    pub ok: bool,
    pub exit_code: i32,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    #[serde(default)]
    pub execution: Option<ToolExecutionRecord>,
}

pub trait ToolExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult>;
}

#[derive(Debug, Clone, Default)]
pub struct LocalToolExecutor;

impl ToolExecutor for LocalToolExecutor {
    fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        let started = Instant::now();
        let result = match call.name {
            ToolName::ReadFile => self.read_file(call),
            ToolName::ListDir => self.list_dir(call),
            ToolName::RunCommand => self.run_command(call),
            ToolName::SearchCode => self.search_code(call),
            ToolName::WriteFile => self.write_file(call),
            ToolName::ReplaceInFile => self.replace_in_file(call),
            ToolName::ApplyPatch => self.apply_patch(call),
        };
        let duration_ms = elapsed_ms_u64(started.elapsed().as_millis());

        Ok(match result {
            Ok(mut ok) => {
                ok.execution = Some(ToolExecutionRecord {
                    tool_name: call.name.as_str().to_string(),
                    args: call.args.clone(),
                    duration_ms,
                    ok: ok.ok,
                    exit_code: ok.exit_code,
                    summary: ok.summary.clone(),
                });
                ok
            }
            Err(err) => {
                let summary = format!("工具执行失败：{err}");
                ToolResult {
                    ok: false,
                    summary: summary.clone(),
                    stdout: String::new(),
                    stderr: err.to_string(),
                    exit_code: 1,
                    execution: Some(ToolExecutionRecord {
                        tool_name: call.name.as_str().to_string(),
                        args: call.args.clone(),
                        duration_ms,
                        ok: false,
                        exit_code: 1,
                        summary,
                    }),
                }
            }
        })
    }
}

impl LocalToolExecutor {
    fn read_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("read_file 缺少路径参数"))?;
        let full_path = resolve_workspace_path(path)?;
        if !full_path.is_file() {
            return Err(anyhow!("read_file 目标不是文件：{}", full_path.display()));
        }

        let content = fs::read(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))?;
        let stdout = String::from_utf8_lossy(&content).to_string();
        let stdout = truncate_output(&stdout);

        Ok(ToolResult {
            ok: true,
            summary: format!("已读取文件：{}", display_rel_path(&full_path)),
            stdout,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    fn list_dir(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call.args.first().map_or(".", String::as_str);
        let full_path = resolve_workspace_path(path)?;
        if !full_path.is_dir() {
            return Err(anyhow!("list_dir 目标不是目录：{}", full_path.display()));
        }

        let mut items = Vec::new();
        for entry in fs::read_dir(&full_path)
            .with_context(|| format!("读取目录失败：{}", full_path.display()))?
        {
            let entry =
                entry.with_context(|| format!("读取目录项失败：{}", full_path.display()))?;
            let file_type = entry
                .file_type()
                .with_context(|| format!("读取目录项类型失败：{}", full_path.display()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let display = if file_type.is_dir() {
                format!("{name}/")
            } else {
                name
            };
            items.push(display);
        }
        items.sort();

        Ok(ToolResult {
            ok: true,
            summary: format!("目录列表：{}", display_rel_path(&full_path)),
            stdout: truncate_output(&items.join("\n")),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    fn run_command(&self, call: &ToolCall) -> Result<ToolResult> {
        let cmd = call
            .args
            .first()
            .ok_or_else(|| anyhow!("run_command 缺少命令参数"))?;
        if !is_allowed_command(cmd) {
            return Err(anyhow!("不允许执行命令：{cmd}"));
        }

        let timeout_ms = command_timeout_ms();
        let (output, timed_out) = execute_command_with_timeout(
            Command::new(cmd)
                .args(call.args.iter().skip(1))
                .current_dir(workspace_root()?)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        )
        .with_context(|| format!("执行命令失败：{cmd}"))?;

        let mut exit_code = output.status.code().unwrap_or(-1);
        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = output.status.success() && !timed_out;

        let summary = if timed_out {
            exit_code = -1;
            format!("命令执行超时：{cmd} (timeout_ms={timeout_ms})")
        } else if ok {
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

    fn search_code(&self, call: &ToolCall) -> Result<ToolResult> {
        let pattern = call
            .args
            .first()
            .ok_or_else(|| anyhow!("search_code 缺少 pattern 参数"))?
            .trim();
        if pattern.is_empty() {
            return Err(anyhow!("search_code pattern 不能为空"));
        }

        let target = call.args.get(1).map_or(".", String::as_str);
        let full_path = resolve_workspace_path(target)?;
        let timeout_ms = command_timeout_ms();
        let target_text = full_path.display().to_string();

        let rg_result = execute_command_with_timeout(
            Command::new("rg")
                .arg("--line-number")
                .arg("--no-heading")
                .arg("--color")
                .arg("never")
                .arg(pattern)
                .arg(&target_text)
                .current_dir(workspace_root()?)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        );

        let (output, timed_out) = match rg_result {
            Ok(payload) => payload,
            Err(_) => execute_command_with_timeout(
                Command::new("grep")
                    .arg("-R")
                    .arg("-n")
                    .arg(pattern)
                    .arg(&target_text)
                    .current_dir(workspace_root()?)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
                timeout_ms,
            )
            .with_context(|| format!("执行代码检索失败：pattern={pattern}"))?,
        };

        let exit_code = if timed_out {
            -1
        } else {
            output.status.code().unwrap_or(-1)
        };
        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = !timed_out && (output.status.success() || exit_code == 1);
        let summary = if timed_out {
            format!("代码检索超时：pattern={pattern} (timeout_ms={timeout_ms})")
        } else if exit_code == 1 {
            format!("代码检索完成：未找到匹配（pattern={pattern}）")
        } else if ok {
            format!("代码检索成功：pattern={pattern}")
        } else {
            format!("代码检索失败：pattern={pattern} (exit_code={exit_code})")
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

    fn write_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("write_file 缺少路径参数"))?;
        let content = call.args.get(1).cloned().unwrap_or_default();
        let full_path = resolve_workspace_write_path(path)?;
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        fs::write(&full_path, content.as_bytes())
            .with_context(|| format!("写入文件失败：{}", full_path.display()))?;

        Ok(ToolResult {
            ok: true,
            summary: format!("文件写入成功：{}", display_rel_path(&full_path)),
            stdout: format!("written_bytes={}", content.len()),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    fn replace_in_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("replace_in_file 缺少路径参数"))?;
        let old = call
            .args
            .get(1)
            .ok_or_else(|| anyhow!("replace_in_file 缺少 old 参数"))?;
        let new = call
            .args
            .get(2)
            .ok_or_else(|| anyhow!("replace_in_file 缺少 new 参数"))?;
        if old.is_empty() {
            return Err(anyhow!("replace_in_file old 参数不能为空"));
        }

        let full_path = resolve_workspace_write_path(path)?;
        if !full_path.is_file() {
            return Err(anyhow!(
                "replace_in_file 目标不是文件：{}",
                full_path.display()
            ));
        }

        let content = fs::read_to_string(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))?;
        let count = content.matches(old).count();
        if count == 0 {
            return Err(anyhow!("replace_in_file 未找到待替换内容"));
        }

        let replaced = content.replace(old, new);
        fs::write(&full_path, replaced.as_bytes())
            .with_context(|| format!("写入替换结果失败：{}", full_path.display()))?;

        Ok(ToolResult {
            ok: true,
            summary: format!(
                "文件替换成功：{} (replacements={count})",
                display_rel_path(&full_path)
            ),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    fn apply_patch(&self, call: &ToolCall) -> Result<ToolResult> {
        let patch = call
            .args
            .first()
            .ok_or_else(|| anyhow!("apply_patch 缺少 patch 内容参数"))?;
        if patch.trim().is_empty() {
            return Err(anyhow!("apply_patch patch 内容不能为空"));
        }

        let temp_patch = write_temp_patch_file(patch)?;
        let timeout_ms = command_timeout_ms();
        let apply_result = execute_command_with_timeout(
            Command::new("git")
                .arg("apply")
                .arg("--whitespace=nowarn")
                .arg("--recount")
                .arg("--unidiff-zero")
                .arg(&temp_patch)
                .current_dir(workspace_root()?)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        );
        let _ = fs::remove_file(&temp_patch);

        let (output, timed_out) = apply_result.context("执行补丁应用失败")?;
        let exit_code = if timed_out {
            -1
        } else {
            output.status.code().unwrap_or(-1)
        };
        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = !timed_out && output.status.success();
        let summary = if timed_out {
            format!("补丁应用超时 (timeout_ms={timeout_ms})")
        } else if ok {
            "补丁应用成功".to_string()
        } else {
            format!("补丁应用失败 (exit_code={exit_code})")
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
}

fn workspace_root() -> Result<PathBuf> {
    std::env::current_dir().context("读取当前工作目录失败")
}

fn resolve_workspace_path(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }

    let root = workspace_root()?;
    let root_canonical = root
        .canonicalize()
        .with_context(|| format!("解析工作目录失败：{}", root.display()))?;
    let candidate = root.join(raw);
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("解析路径失败：{}", candidate.display()))?;

    if !canonical.starts_with(&root_canonical) {
        return Err(anyhow!("路径越界，超出工作目录：{}", canonical.display()));
    }
    Ok(canonical)
}

fn resolve_workspace_write_path(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("路径参数不能为空"));
    }

    let root = workspace_root()?;
    let root_canonical = root
        .canonicalize()
        .with_context(|| format!("解析工作目录失败：{}", root.display()))?;
    let candidate = root.join(raw);
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

    if !parent_canonical.starts_with(&root_canonical) {
        return Err(anyhow!("路径越界，超出工作目录：{}", candidate.display()));
    }
    Ok(candidate)
}

fn write_temp_patch_file(patch: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("tiangong-{}.patch", scru128::new()));
    fs::write(&path, patch.as_bytes())
        .with_context(|| format!("写入临时补丁文件失败：{}", path.display()))?;
    Ok(path)
}

fn display_rel_path(path: &Path) -> String {
    let root = match workspace_root().and_then(|root| {
        root.canonicalize()
            .with_context(|| format!("解析工作目录失败：{}", root.display()))
    }) {
        Ok(root) => root,
        Err(_) => return path.display().to_string(),
    };

    path.strip_prefix(&root)
        .map(|rel| rel.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn is_allowed_command(cmd: &str) -> bool {
    matches!(cmd, "echo" | "pwd" | "ls" | "cat" | "head" | "tail" | "wc")
}

fn command_timeout_ms() -> u64 {
    const DEFAULT_TIMEOUT_MS: u64 = 10_000;
    std::env::var("TOOL_COMMAND_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
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

fn elapsed_ms_u64(raw: u128) -> u64 {
    raw.min(u64::MAX as u128) as u64
}

fn truncate_output(raw: &str) -> String {
    const MAX_CHARS: usize = 6000;
    let mut output = raw.chars().take(MAX_CHARS).collect::<String>();
    if raw.chars().count() > MAX_CHARS {
        output.push_str("\n...(truncated)");
    }
    output
}
