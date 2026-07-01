//! 基础文件工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，直接从 LLM 传入的命名参数
//! JSON（`call.arguments`）按 key 取参，绕开旧的「位置参数数组」模式。
//!
//! 路径解析使用 core 暴露的 `*_with_base` 变体，显式传入由 core 注入的会话工作目录，
//! 不依赖 thread-local SESSION_CWD。

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use chrono::{Local, SecondsFormat};
use serde_json::{Value, json};
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool::common as shared;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::plugin::FsPlugin;

/// 工具名常量。
const TOOL_LIST_DIR: &str = "list_dir";
const TOOL_TREE_DIR: &str = "tree_dir";
const TOOL_READ_FILE: &str = "read_file";
const TOOL_SEARCH_CODE: &str = "search_code";
const TOOL_CURRENT_TIME: &str = "current_time";
const TOOL_WRITE_FILE: &str = "write_file";
const TOOL_REPLACE_IN_FILE: &str = "replace_in_file";
const TOOL_APPLY_PATCH: &str = "apply_patch";

const DEFAULT_TREE_MAX_DEPTH: usize = 2;
const MAX_TREE_MAX_DEPTH: usize = 8;
const MAX_TREE_NODES: usize = 1200;
const DEFAULT_READ_MAX_LINES: usize = 200;
const MAX_READ_MAX_LINES: usize = 2000;

impl FsPlugin {
    /// 取当前工作目录，未注入时报错（fs 工具必须知道 workspace）。
    fn base(&self) -> Result<std::path::PathBuf> {
        self.workspace()
            .ok_or_else(|| anyhow!("会话工作目录未注入，无法执行文件工具"))
    }

    /// 主分发入口：按 `call.name` 路由到对应处理函数。
    pub(crate) fn dispatch(&self, call: &ToolCall) -> Option<ToolResult> {
        let result = match call.name.as_str() {
            TOOL_LIST_DIR => self.handle_list_dir(call),
            TOOL_TREE_DIR => self.handle_tree_dir(call),
            TOOL_READ_FILE => self.handle_read_file(call),
            TOOL_SEARCH_CODE => self.handle_search_code(call),
            TOOL_CURRENT_TIME => self.handle_current_time(),
            TOOL_WRITE_FILE => self.handle_write_file(call),
            TOOL_REPLACE_IN_FILE => self.handle_replace_in_file(call),
            TOOL_APPLY_PATCH => self.handle_apply_patch(call),
            _ => return None,
        };
        Some(result)
    }

    // ── list_dir ────────────────────────────────────────────────

    fn handle_list_dir(&self, call: &ToolCall) -> ToolResult {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return tool_error("list_dir", e),
        };
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".");
        let full_path = match self.resolve_read_path(path, &base) {
            Ok(p) => p,
            Err(e) => return tool_error("list_dir", e),
        };
        if !full_path.is_dir() {
            return tool_error("list_dir", anyhow!("目标不是目录：{}", full_path.display()));
        }

        let mut items = Vec::new();
        let read_dir = match fs::read_dir(&full_path)
            .with_context(|| format!("读取目录失败：{}", full_path.display()))
        {
            Ok(rd) => rd,
            Err(e) => return tool_error("list_dir", anyhow!(e)),
        };
        for entry in read_dir {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => return tool_error("list_dir", anyhow!(e)),
            };
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = entry.file_name().to_string_lossy().to_string();
            items.push(if is_dir { format!("{name}/") } else { name });
        }
        items.sort();

        ToolResult {
            ok: true,
            summary: format!(
                "目录列表：{}",
                shared::display_rel_path_with(&full_path, &base)
            ),
            stdout: items.join("\n"),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    // ── tree_dir ────────────────────────────────────────────────

    fn handle_tree_dir(&self, call: &ToolCall) -> ToolResult {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return tool_error("tree_dir", e),
        };
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".");
        let max_depth = call
            .arguments
            .get("max_depth")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_TREE_MAX_DEPTH)
            .min(MAX_TREE_MAX_DEPTH);

        let full_path = match self.resolve_read_path(path, &base) {
            Ok(p) => p,
            Err(e) => return tool_error("tree_dir", e),
        };
        if !full_path.is_dir() {
            return tool_error("tree_dir", anyhow!("目标不是目录：{}", full_path.display()));
        }

        let rel = shared::display_rel_path_with(&full_path, &base);
        let mut lines = vec![if rel == "." {
            "./".to_string()
        } else {
            format!("{rel}/")
        }];
        let mut visited = 0usize;
        let mut truncated = false;
        if let Err(e) = append_tree_lines(
            &full_path,
            0,
            max_depth,
            "",
            &mut lines,
            &mut visited,
            &mut truncated,
        ) {
            return tool_error("tree_dir", e);
        }
        if truncated {
            lines.push(format!(
                "...(节点数量超过限制，已截断，max_nodes={MAX_TREE_NODES})"
            ));
        }

        ToolResult {
            ok: true,
            summary: format!("目录树：{} (max_depth={max_depth})", rel),
            stdout: lines.join("\n"),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    // ── read_file ───────────────────────────────────────────────

    fn handle_read_file(&self, call: &ToolCall) -> ToolResult {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return tool_error("read_file", e),
        };
        let Some(path) = call.arguments.get("path").and_then(Value::as_str) else {
            return param_error("read_file 缺少 path 参数");
        };
        let start_line = call
            .arguments
            .get("start_line")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(1)
            .max(1);
        let max_lines = call
            .arguments
            .get("max_lines")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_READ_MAX_LINES)
            .clamp(1, MAX_READ_MAX_LINES);

        let full_path = match self.resolve_read_path(path, &base) {
            Ok(p) => p,
            Err(e) => return tool_error("read_file", e),
        };
        if !full_path.is_file() {
            return tool_error(
                "read_file",
                anyhow!("目标不是文件：{}", full_path.display()),
            );
        }

        let content = match fs::read_to_string(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))
        {
            Ok(c) => c,
            Err(e) => return tool_error("read_file", anyhow!(e)),
        };
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start_idx = (start_line - 1).min(total);
        let end_idx = (start_idx + max_lines).min(total);
        let stdout = lines[start_idx..end_idx]
            .iter()
            .enumerate()
            .map(|(idx, line)| format!("{:>6}\t{}", start_idx + idx + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        ToolResult {
            ok: true,
            summary: format!(
                "已读取文件：{} (range={}..{}, total_lines={})",
                shared::display_rel_path_with(&full_path, &base),
                start_idx + 1,
                end_idx,
                total
            ),
            stdout,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    // ── search_code ─────────────────────────────────────────────

    fn handle_search_code(&self, call: &ToolCall) -> ToolResult {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return tool_error("search_code", e),
        };
        let Some(pattern) = call.arguments.get("pattern").and_then(Value::as_str) else {
            return param_error("search_code 缺少 pattern 参数");
        };
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return param_error("search_code pattern 不能为空");
        }
        let target = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".");
        let full_path = match self.resolve_read_path(target, &base) {
            Ok(p) => p,
            Err(e) => return tool_error("search_code", e),
        };

        let timeout_ms = shared::command_timeout_ms();
        let target_text = full_path.display().to_string();
        let rg_result = shared::execute_command_with_timeout(
            Command::new("rg")
                .arg("--line-number")
                .arg("--no-heading")
                .arg("--color")
                .arg("never")
                .arg(pattern)
                .arg(&target_text)
                .current_dir(&base)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            timeout_ms,
        );

        let (output, timed_out) = match rg_result {
            Ok(payload) => payload,
            Err(_) => match shared::execute_command_with_timeout(
                Command::new("grep")
                    .arg("-R")
                    .arg("-n")
                    .arg(pattern)
                    .arg(&target_text)
                    .current_dir(&base)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
                timeout_ms,
            )
            .with_context(|| format!("执行代码检索失败：pattern={pattern}"))
            {
                Ok(p) => p,
                Err(e) => return tool_error("search_code", anyhow!(e)),
            },
        };

        let exit_code = if timed_out {
            -1
        } else {
            output.status.code().unwrap_or(-1)
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = if timed_out {
            shared::truncate_output(&stdout)
        } else {
            stdout
        };
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

        ToolResult {
            ok,
            summary,
            stdout,
            stderr,
            exit_code,
            execution: None,
        }
    }

    // ── current_time ────────────────────────────────────────────

    fn handle_current_time(&self) -> ToolResult {
        let now = Local::now();
        let output = json!({
            "local_time": now.naive_local().to_string(),
            "rfc3339": now.to_rfc3339_opts(SecondsFormat::Secs, false),
            "unix_timestamp": now.timestamp(),
            "timezone_offset": now.offset().to_string(),
        });
        ToolResult {
            ok: true,
            summary: format!("当前本地时间：{}", now.naive_local()),
            stdout: serde_json::to_string_pretty(&output).unwrap_or_default(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    // ── write_file ──────────────────────────────────────────────

    fn handle_write_file(&self, call: &ToolCall) -> ToolResult {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return tool_error("write_file", e),
        };
        let Some(path) = call.arguments.get("path").and_then(Value::as_str) else {
            return param_error("write_file 缺少 path 参数");
        };
        let content = call
            .arguments
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let append = call
            .arguments
            .get("append")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let full_path = match self.resolve_write_path(path, &base) {
            Ok(p) => p,
            Err(e) => return tool_error("write_file", e),
        };
        if let Some(parent) = full_path.parent() {
            if let Err(e) = fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))
            {
                return tool_error("write_file", anyhow!(e));
            }
        }
        if append {
            use std::fs::OpenOptions;
            use std::io::Write;
            let mut file = match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&full_path)
                .with_context(|| format!("追加打开文件失败：{}", full_path.display()))
            {
                Ok(f) => f,
                Err(e) => return tool_error("write_file", anyhow!(e)),
            };
            if let Err(e) = file
                .write_all(content.as_bytes())
                .with_context(|| format!("追加写入文件失败：{}", full_path.display()))
            {
                return tool_error("write_file", anyhow!(e));
            }
        } else if let Err(e) = atomic_write_file(&full_path, content.as_bytes()) {
            return tool_error("write_file", e);
        }

        ToolResult {
            ok: true,
            summary: format!(
                "文件写入成功：{} (mode={})",
                shared::display_rel_path_with(&full_path, &base),
                if append { "append" } else { "overwrite-atomic" }
            ),
            stdout: format!("written_bytes={},append={append}", content.len()),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    // ── replace_in_file ─────────────────────────────────────────

    fn handle_replace_in_file(&self, call: &ToolCall) -> ToolResult {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return tool_error("replace_in_file", e),
        };
        let Some(path) = call.arguments.get("path").and_then(Value::as_str) else {
            return param_error("replace_in_file 缺少 path 参数");
        };
        let Some(old) = call.arguments.get("old").and_then(Value::as_str) else {
            return param_error("replace_in_file 缺少 old 参数");
        };
        let new = call
            .arguments
            .get("new")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let replace_all = call
            .arguments
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let expected_count = call
            .arguments
            .get("expected_count")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        if old.is_empty() {
            return param_error("replace_in_file old 参数不能为空");
        }

        let full_path = match self.resolve_write_path(path, &base) {
            Ok(p) => p,
            Err(e) => return tool_error("replace_in_file", e),
        };
        if !full_path.is_file() {
            return tool_error(
                "replace_in_file",
                anyhow!("目标不是文件：{}", full_path.display()),
            );
        }

        let content = match fs::read_to_string(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))
        {
            Ok(c) => c,
            Err(e) => return tool_error("replace_in_file", anyhow!(e)),
        };
        let count = content.matches(old).count();
        if count == 0 {
            return tool_error("replace_in_file", anyhow!("未找到待替换内容"));
        }
        if let Some(expected) = expected_count
            && count != expected
        {
            return tool_error(
                "replace_in_file",
                anyhow!("命中数量不符合预期：expected={expected}, actual={count}"),
            );
        }

        let (replaced, replaced_count) = if replace_all {
            (content.replace(old, new), count)
        } else {
            if count != 1 {
                return tool_error(
                    "replace_in_file",
                    anyhow!(
                        "默认仅允许单点替换，当前命中 {} 处；如需全量替换请设置 replace_all=true",
                        count
                    ),
                );
            }
            (content.replacen(old, new, 1), 1)
        };
        if let Err(e) = fs::write(&full_path, replaced.as_bytes())
            .with_context(|| format!("写入替换结果失败：{}", full_path.display()))
        {
            return tool_error("replace_in_file", anyhow!(e));
        }

        ToolResult {
            ok: true,
            summary: format!(
                "文件替换成功：{} (replacements={}, replace_all={})",
                shared::display_rel_path_with(&full_path, &base),
                replaced_count,
                replace_all
            ),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    // ── apply_patch ─────────────────────────────────────────────

    fn handle_apply_patch(&self, call: &ToolCall) -> ToolResult {
        let base = match self.base() {
            Ok(b) => b,
            Err(e) => return tool_error("apply_patch", e),
        };
        let Some(patch) = call.arguments.get("patch").and_then(Value::as_str) else {
            return param_error("apply_patch 缺少 patch 参数");
        };
        if patch.trim().is_empty() {
            return param_error("apply_patch patch 内容不能为空");
        }
        let verify = call
            .arguments
            .get("verify")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let workdir_raw = call.arguments.get("workdir").and_then(Value::as_str);

        let effective_cwd = match shared::resolve_effective_cwd_with(workdir_raw, &base) {
            Ok(c) => c,
            Err(e) => return tool_error("apply_patch", e),
        };
        let stats = match apply_unified_diff_patch(patch, &effective_cwd, verify) {
            Ok(s) => s,
            Err(e) => return tool_error("apply_patch", e),
        };
        let summary = format!(
            "补丁{}成功：add={}, delete={}, update={}, move={}",
            if verify { "校验" } else { "应用" },
            stats.added,
            stats.deleted,
            stats.updated,
            stats.moved
        );
        let stdout = json!({
            "verify": verify,
            "effective_cwd": effective_cwd.display().to_string(),
            "counts": {
                "add": stats.added,
                "delete": stats.deleted,
                "update": stats.updated,
                "move": stats.moved,
            },
            "files": stats.files,
        })
        .to_string();

        ToolResult {
            ok: true,
            summary,
            stdout: shared::truncate_output(&stdout),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    // ── 路径解析 helper ─────────────────────────────────────────

    /// 读路径解析（list_dir / read_file / tree_dir / search_code 用）。
    fn resolve_read_path(&self, raw: &str, base: &Path) -> Result<std::path::PathBuf> {
        if self.is_full_trust() {
            shared::resolve_workspace_path_trusted_with(raw, base)
        } else {
            shared::resolve_workspace_path_with(raw, base)
        }
    }

    /// 写路径解析（write_file / replace_in_file 用）。
    fn resolve_write_path(&self, raw: &str, base: &Path) -> Result<std::path::PathBuf> {
        if self.is_full_trust() {
            // 信任模式仍然限制写入范围
            shared::resolve_write_path_from_base(raw, base)
        } else {
            shared::resolve_write_path_from_base(raw, base)
        }
    }
}

impl ToolSpecProvider for FsPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: TOOL_LIST_DIR.to_string(),
                description: "列出目录中的文件和子目录".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "目录路径，默认当前目录" }
                    },
                    "required": []
                }),
            },
            ToolSpec {
                name: TOOL_TREE_DIR.to_string(),
                description: "按目录树格式列出目录，支持通过 max_depth 限制遍历深度".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "目录路径，默认当前目录" },
                        "max_depth": {
                            "type": "integer",
                            "description": "遍历最大深度，建议 1-4，默认 2，最大 8",
                            "minimum": 0,
                            "maximum": 8
                        }
                    },
                    "required": []
                }),
            },
            ToolSpec {
                name: TOOL_READ_FILE.to_string(),
                description: "读取文件内容，支持按行范围读取".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径" },
                        "start_line": { "type": "integer", "description": "起始行（从 1 开始，默认 1）", "minimum": 1 },
                        "max_lines": { "type": "integer", "description": "最大读取行数（默认 200，最大 2000）", "minimum": 1, "maximum": 2000 }
                    },
                    "required": ["path"]
                }),
            },
            ToolSpec {
                name: TOOL_SEARCH_CODE.to_string(),
                description: "在目录中检索文本（优先使用 rg）".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "检索文本或正则模式" },
                        "path": { "type": "string", "description": "目标目录或文件路径，默认当前目录" }
                    },
                    "required": ["pattern"]
                }),
            },
            ToolSpec {
                name: TOOL_CURRENT_TIME.to_string(),
                description: "获取当前本地时间、RFC3339 时间、Unix 时间戳和时区偏移。涉及今天、现在、当前时间、日期换算等请求时使用。".to_string(),
                input_schema: json!({ "type": "object", "properties": {}, "required": [] }),
            },
            ToolSpec {
                name: TOOL_WRITE_FILE.to_string(),
                description: "写入文件内容（支持覆盖或追加）".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径" },
                        "content": { "type": "string", "description": "要写入的内容" },
                        "append": { "type": "boolean", "description": "是否追加写入，默认 false（覆盖）" }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolSpec {
                name: TOOL_REPLACE_IN_FILE.to_string(),
                description: "在文件中将旧文本替换为新文本，默认仅允许单点替换".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径" },
                        "old": { "type": "string", "description": "待替换的旧文本" },
                        "new": { "type": "string", "description": "替换后的新文本" },
                        "replace_all": { "type": "boolean", "description": "是否替换全部命中，默认 false" },
                        "expected_count": { "type": "integer", "description": "预期命中数量（可选）", "minimum": 1 }
                    },
                    "required": ["path", "old", "new"]
                }),
            },
            ToolSpec {
                name: TOOL_APPLY_PATCH.to_string(),
                description: "对文件应用补丁文本，仅支持 unified diff（---/+++/@@）".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "patch": { "type": "string", "description": "补丁内容文本（unified diff）" },
                        "verify": { "type": "boolean", "description": "是否仅校验不落盘（dry-run）" },
                        "workdir": { "type": "string", "description": "补丁工作目录（可选，默认当前工作目录）" }
                    },
                    "required": ["patch"]
                }),
            },
        ]
    }
}

impl ToolOverrideHandler for FsPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let result = self.dispatch(call);
        Box::pin(async move { result })
    }
}

// ── 辅助函数 ─────────────────────────────────────────────────────

fn param_error(msg: &str) -> ToolResult {
    ToolResult {
        ok: false,
        summary: msg.to_string(),
        stdout: String::new(),
        stderr: msg.to_string(),
        exit_code: 1,
        execution: None,
    }
}

fn tool_error(tool: &str, e: anyhow::Error) -> ToolResult {
    let summary = format!("{tool} 失败：{e}");
    ToolResult {
        ok: false,
        summary: summary.clone(),
        stdout: String::new(),
        stderr: summary,
        exit_code: 1,
        execution: None,
    }
}

fn atomic_write_file(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("无法确定目标文件父目录：{}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("目标文件名非法：{}", path.display()))?;
    let temp_path = parent.join(format!(".{}.tmp-{}", file_name, scru128::new()));
    fs::write(&temp_path, content)
        .with_context(|| format!("写入临时文件失败：{}", temp_path.display()))?;
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("删除旧文件失败：{}", path.display()))?;
    }
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "原子替换失败：temp={}, target={}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn append_tree_lines(
    path: &Path,
    current_depth: usize,
    max_depth: usize,
    prefix: &str,
    lines: &mut Vec<String>,
    visited: &mut usize,
    truncated: &mut bool,
) -> Result<()> {
    if current_depth >= max_depth {
        return Ok(());
    }
    if *visited >= MAX_TREE_NODES {
        *truncated = true;
        return Ok(());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("读取目录失败：{}", path.display()))?
    {
        let entry = entry.with_context(|| format!("读取目录项失败：{}", path.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry
            .file_type()
            .with_context(|| format!("读取目录项类型失败：{}", path.display()))?
            .is_dir();
        entries.push((name, is_dir, entry.path()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let total = entries.len();
    for (idx, (name, is_dir, child_path)) in entries.into_iter().enumerate() {
        if *visited >= MAX_TREE_NODES {
            *truncated = true;
            return Ok(());
        }

        *visited += 1;
        let last = idx + 1 == total;
        let branch = if last { "`-- " } else { "|-- " };
        let display = if is_dir { format!("{name}/") } else { name };
        lines.push(format!("{prefix}{branch}{display}"));

        if is_dir {
            let next_prefix = if last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}|   ")
            };
            append_tree_lines(
                &child_path,
                current_depth + 1,
                max_depth,
                &next_prefix,
                lines,
                visited,
                truncated,
            )?;
            if *truncated {
                return Ok(());
            }
        }
    }
    Ok(())
}

// ── apply_patch 实现（unified diff）──────────────────────────────

#[derive(Debug, Default)]
struct PatchStats {
    added: usize,
    deleted: usize,
    updated: usize,
    moved: usize,
    files: Vec<Value>,
}

fn apply_unified_diff_patch(patch: &str, effective_cwd: &Path, verify: bool) -> Result<PatchStats> {
    use diffy::{Patch, apply as diffy_apply};

    let sections = split_unified_diff_sections(patch)?;
    let mut stats = PatchStats::default();

    for section in &sections {
        let parsed =
            Patch::from_str(section).map_err(|err| anyhow!("解析 unified diff 失败：{err}"))?;
        let original = normalize_diff_filename(parsed.original().unwrap_or_default())?;
        let modified = normalize_diff_filename(parsed.modified().unwrap_or_default())?;

        let is_add = original == "/dev/null" && modified != "/dev/null";
        let is_delete = modified == "/dev/null" && original != "/dev/null";

        if is_add {
            let target = shared::resolve_write_path_from_base(&modified, effective_cwd)?;
            if target.exists() {
                return Err(anyhow!("新增文件已存在：{}", target.display()));
            }
            let content =
                diffy_apply("", &parsed).map_err(|err| anyhow!("新增文件补丁应用失败：{err}"))?;
            if !verify {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|err| anyhow!("创建目录失败：{}，{err}", parent.display()))?;
                }
                fs::write(&target, content.as_bytes())
                    .map_err(|err| anyhow!("写入新增文件失败：{}，{err}", target.display()))?;
            }
            stats.added += 1;
            stats.files.push(json!({
                "action": "add",
                "target": shared::display_rel_path_with(&target, effective_cwd),
            }));
            continue;
        }

        if is_delete {
            let source = shared::resolve_write_path_from_base(&original, effective_cwd)?;
            if !source.is_file() {
                return Err(anyhow!("删除目标不是文件：{}", source.display()));
            }
            let base = fs::read_to_string(&source)
                .map_err(|err| anyhow!("读取删除目标失败：{}，{err}", source.display()))?;
            let content =
                diffy_apply(&base, &parsed).map_err(|err| anyhow!("删除补丁应用失败：{err}"))?;
            if !content.is_empty() {
                return Err(anyhow!(
                    "删除补丁校验失败：应用后内容非空：{}",
                    source.display()
                ));
            }
            if !verify {
                fs::remove_file(&source)
                    .map_err(|err| anyhow!("删除文件失败：{}，{err}", source.display()))?;
            }
            stats.deleted += 1;
            stats.files.push(json!({
                "action": "delete",
                "source": shared::display_rel_path_with(&source, effective_cwd),
            }));
            continue;
        }

        let source = shared::resolve_write_path_from_base(&original, effective_cwd)?;
        let target = shared::resolve_write_path_from_base(&modified, effective_cwd)?;
        if !source.is_file() {
            return Err(anyhow!("修改目标不是文件：{}", source.display()));
        }
        let base = fs::read_to_string(&source)
            .map_err(|err| anyhow!("读取文件失败：{}，{err}", source.display()))?;
        let content =
            diffy_apply(&base, &parsed).map_err(|err| anyhow!("修改补丁应用失败：{err}"))?;

        if !verify {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| anyhow!("创建目录失败：{}，{err}", parent.display()))?;
            }
            fs::write(&target, content.as_bytes())
                .map_err(|err| anyhow!("写入文件失败：{}，{err}", target.display()))?;
            if source != target {
                fs::remove_file(&source)
                    .map_err(|err| anyhow!("删除原文件失败：{}，{err}", source.display()))?;
            }
        }

        stats.updated += 1;
        if source != target {
            stats.moved += 1;
        }
        stats.files.push(json!({
            "action": if source == target { "update" } else { "move_update" },
            "source": shared::display_rel_path_with(&source, effective_cwd),
            "target": shared::display_rel_path_with(&target, effective_cwd),
        }));
    }

    Ok(stats)
}

fn split_unified_diff_sections(patch: &str) -> Result<Vec<String>> {
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.len() < 3 {
        return Err(anyhow!("unified diff 内容过短，无法解析"));
    }

    let mut section_starts = Vec::new();
    for idx in 0..(lines.len() - 1) {
        if lines[idx].starts_with("--- ") && lines[idx + 1].starts_with("+++ ") {
            section_starts.push(idx);
        }
    }
    if section_starts.is_empty() {
        return Err(anyhow!("unified diff 缺少文件头（--- / +++）"));
    }

    let mut sections = Vec::new();
    for (index, start) in section_starts.iter().enumerate() {
        let end = section_starts
            .get(index + 1)
            .copied()
            .unwrap_or(lines.len());
        let mut section = lines[*start..end].join("\n");
        if !section.ends_with('\n') {
            section.push('\n');
        }
        sections.push(section);
    }
    Ok(sections)
}

fn normalize_diff_filename(raw: &str) -> Result<String> {
    let path = raw.trim();
    if path.is_empty() {
        return Err(anyhow!("unified diff 文件路径为空"));
    }
    if path == "/dev/null" {
        return Ok(path.to_string());
    }
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .trim();
    if path.is_empty() {
        return Err(anyhow!("unified diff 文件路径非法"));
    }
    Ok(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiangong_core::core::Plugin;
    use tiangong_core::model::ToolCall;

    fn make_plugin(dir: &tempfile::TempDir) -> FsPlugin {
        let plugin = FsPlugin::new();
        plugin.set_workspace(dir.path());
        plugin
    }

    fn make_call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: "test".to_string(),
            name: name.to_string(),
            arguments: args,
        }
    }

    #[test]
    fn write_read_replace_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);

        // write_file
        let r = plugin
            .dispatch(&make_call(
                TOOL_WRITE_FILE,
                json!({ "path": "a.txt", "content": "hello world\nhello again" }),
            ))
            .unwrap();
        assert!(r.ok, "{}", r.summary);

        // read_file
        let r = plugin
            .dispatch(&make_call(TOOL_READ_FILE, json!({ "path": "a.txt" })))
            .unwrap();
        assert!(r.ok);
        assert!(r.stdout.contains("hello world"));

        // replace_in_file
        let r = plugin
            .dispatch(&make_call(
                TOOL_REPLACE_IN_FILE,
                json!({ "path": "a.txt", "old": "hello", "new": "hi", "replace_all": true }),
            ))
            .unwrap();
        assert!(r.ok, "{}", r.summary);

        let r = plugin
            .dispatch(&make_call(TOOL_READ_FILE, json!({ "path": "a.txt" })))
            .unwrap();
        assert!(r.stdout.contains("hi world"));
        assert!(!r.stdout.contains("hello"));
    }

    #[test]
    fn list_and_tree_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        std::fs::write(dir.path().join("f1.txt"), "1").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("f2.txt"), "2").unwrap();

        let r = plugin
            .dispatch(&make_call(TOOL_LIST_DIR, json!({})))
            .unwrap();
        assert!(r.ok);
        assert!(r.stdout.contains("f1.txt"));
        assert!(r.stdout.contains("sub/"));

        let r = plugin
            .dispatch(&make_call(TOOL_TREE_DIR, json!({ "max_depth": 2 })))
            .unwrap();
        assert!(r.ok);
        assert!(r.stdout.contains("f1.txt"));
        assert!(r.stdout.contains("sub/"));
        assert!(r.stdout.contains("f2.txt"));
    }

    #[test]
    fn current_time_works() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let r = plugin
            .dispatch(&make_call(TOOL_CURRENT_TIME, json!({})))
            .unwrap();
        assert!(r.ok);
        assert!(r.stdout.contains("unix_timestamp"));
    }

    #[test]
    fn apply_patch_add_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        let patch = "--- /dev/null\n+++ new.txt\n@@ -0,0 +1 @@\n+hello\n";
        let r = plugin
            .dispatch(&make_call(TOOL_APPLY_PATCH, json!({ "patch": patch })))
            .unwrap();
        assert!(r.ok, "{}", r.summary);
        assert!(dir.path().join("new.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello\n"
        );
    }

    #[test]
    fn unknown_tool_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let plugin = make_plugin(&dir);
        assert!(
            plugin
                .dispatch(&make_call("not_an_fs_tool", json!({})))
                .is_none()
        );
    }
}
