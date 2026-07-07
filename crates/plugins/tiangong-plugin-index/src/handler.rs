//! 索引搜索工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，提供 `index_search` 与
//! `search_code` 两个工具。参数直接从 LLM 传入的命名参数 JSON（`call.arguments`）
//! 按 key 取参。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::index::{IndexQuery, IndexScope};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool::common as shared;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};

use crate::plugin::IndexPlugin;

/// 工具名常量。
const TOOL_INDEX_SEARCH: &str = "index_search";
const TOOL_SEARCH_CODE: &str = "search_code";

impl IndexPlugin {
    /// 主分发入口：按 `call.name` 路由到对应处理函数。
    pub(crate) fn dispatch(&self, call: &ToolCall, session_id: &str) -> Option<ToolResult> {
        let result = match call.name.as_str() {
            TOOL_INDEX_SEARCH => self.handle_index_search(call, session_id),
            TOOL_SEARCH_CODE => self.handle_search_code(call),
            _ => return None,
        };
        Some(result)
    }

    // ── index_search ────────────────────────────────────────────

    fn handle_index_search(&self, call: &ToolCall, session_id: &str) -> ToolResult {
        let started = Instant::now();

        let Some(im) = self.index_manager() else {
            return index_search_result(
                false,
                "索引系统未初始化",
                String::new(),
                "index manager not available".to_string(),
                1,
                started,
            );
        };

        let query = call
            .arguments
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return index_search_result(
                false,
                "查询为空",
                String::new(),
                "query parameter is required".to_string(),
                1,
                started,
            );
        }

        let scope_str = call
            .arguments
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("all");
        let scope = match scope_str {
            "workspace" => IndexScope::Workspace,
            "session" => IndexScope::Session,
            _ => IndexScope::All,
        };

        let limit = call
            .arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(1, 20) as usize;

        let mut stdout_parts: Vec<String> = Vec::new();

        // Workspace 索引查询
        if scope == IndexScope::Workspace || scope == IndexScope::All {
            if let Some(cwd) = self.workspace() {
                if cwd.is_dir() {
                    let index_query = IndexQuery::new(query)
                        .with_scope(IndexScope::Workspace)
                        .with_limit(limit);
                    match im.search(&cwd, &index_query) {
                        Ok(hits) if !hits.is_empty() => {
                            stdout_parts.push("【工作区文件】".to_string());
                            for hit in &hits {
                                stdout_parts.push(format!("- {} ({})", hit.path, hit.language));
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            stdout_parts.push(format!("【工作区搜索失败: {e}】"));
                        }
                    }
                }
            }
        }

        // Session 索引查询
        if scope == IndexScope::Session || scope == IndexScope::All {
            match im.search_session(session_id, query, limit) {
                Ok(hits) if !hits.is_empty() => {
                    stdout_parts.push("【对话历史】".to_string());
                    for hit in &hits {
                        let preview: String = hit.content.chars().take(200).collect();
                        stdout_parts.push(format!("- [{}] {}", hit.role, preview));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    stdout_parts.push(format!("【对话搜索失败: {e}】"));
                }
            }
        }

        if stdout_parts.is_empty() {
            index_search_result(
                true,
                format!("未找到与 \"{query}\" 相关的索引结果"),
                String::new(),
                String::new(),
                0,
                started,
            )
        } else {
            let stdout = stdout_parts.join("\n");
            let count = stdout_parts.iter().filter(|l| l.starts_with('-')).count();
            index_search_result(
                true,
                format!("找到 {count} 条索引结果"),
                stdout,
                String::new(),
                0,
                started,
            )
        }
    }

    // ── search_code ─────────────────────────────────────────────

    fn handle_search_code(&self, call: &ToolCall) -> ToolResult {
        let base = match self.workspace() {
            Some(b) => b,
            None => return tool_error("search_code", anyhow!("会话工作目录未注入，无法执行检索")),
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
                    .arg("-I")
                    .arg("--exclude-dir=.git")
                    .arg("--exclude-dir=target")
                    .arg("--exclude-dir=node_modules")
                    .arg("--exclude-dir=dist")
                    .arg("--exclude-dir=build")
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
        // 无论是否超时都截断：命中很多时（如搜索 use/fn）输出可能巨大，
        // 避免大块文本进入 ToolResult / session / LLM 上下文。
        let stdout = shared::truncate_output(&stdout);
        let stderr = shared::truncate_output(&stderr);
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

    // ── 路径解析 helper ─────────────────────────────────────────

    /// 读路径解析（search_code 用），与 fs 插件保持一致的信任模式语义。
    fn resolve_read_path(&self, raw: &str, base: &Path) -> Result<std::path::PathBuf> {
        if self.is_full_trust() {
            shared::resolve_workspace_path_trusted_with(raw, base)
        } else {
            shared::resolve_workspace_path_with(raw, base)
        }
    }
}

impl ToolSpecProvider for IndexPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: TOOL_INDEX_SEARCH.to_string(),
                description: "搜索当前工作区的文件内容和对话历史索引。查找代码文件、符号定义或之前的对话内容时优先使用此工具，需要精确定位代码行时配合 search_code 使用。仅在索引结果不足时再使用 recall_memory。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "搜索关键词，支持文件路径、代码片段、符号名、对话内容"
                        },
                        "scope": {
                            "type": "string",
                            "enum": ["workspace", "session", "all"],
                            "description": "搜索范围：workspace=仅文件索引，session=仅对话索引，all=全部（默认 all）"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "最多返回多少条结果，默认 10，最大 20",
                            "minimum": 1,
                            "maximum": 20
                        }
                    },
                    "required": ["query"]
                }),
            },
            ToolSpec {
                name: TOOL_SEARCH_CODE.to_string(),
                description: "在目录中精确检索文本（优先使用 ripgrep/rg，rg 缺失时回退到 grep 较慢；需精确定位代码行时与 index_search 配合）".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "检索文本或正则模式" },
                        "path": { "type": "string", "description": "目标目录或文件路径，默认当前目录。非完全信任模式下限制在工作区内；完全信任模式下可读取工作区外路径" }
                    },
                    "required": ["pattern"]
                }),
            },
        ]
    }
}

impl ToolOverrideHandler for IndexPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &tiangong_core::session::Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let result = self.dispatch(call, &session.id);
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

fn index_search_result(
    ok: bool,
    summary: impl Into<String>,
    stdout: String,
    stderr: String,
    exit_code: i32,
    started: Instant,
) -> ToolResult {
    let summary = summary.into();
    ToolResult {
        ok,
        summary: summary.clone(),
        stdout,
        stderr,
        exit_code,
        execution: Some(tiangong_core::tool::ToolExecutionRecord {
            tool_name: "index_search".to_string(),
            args: Vec::new(),
            duration_ms: started.elapsed().as_millis() as u64,
            ok,
            exit_code,
            summary,
        }),
    }
}
