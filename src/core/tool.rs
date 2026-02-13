use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolName {
    ReadFile,
    ListDir,
    RunCommand,
}

impl ToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::ListDir => "list_dir",
            Self::RunCommand => "run_command",
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

        let output = Command::new(cmd)
            .args(call.args.iter().skip(1))
            .current_dir(workspace_root()?)
            .output()
            .with_context(|| format!("执行命令失败：{cmd}"))?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout));
        let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr));
        let ok = output.status.success();

        Ok(ToolResult {
            ok,
            summary: if ok {
                format!("命令执行成功：{cmd}")
            } else {
                format!("命令执行失败：{cmd} (exit_code={exit_code})")
            },
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
