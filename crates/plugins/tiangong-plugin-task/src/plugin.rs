//! task 插件：后台任务管理（spawn_task / query_task / list_tasks / cancel_task / wait_tasks）。
//!
//! 原 `tiangong-core::runtime::handle_background_task` + `inject_enhanced_tools` 的后台
//! 任务 spec，随收敛重构迁出为独立插件（#208）。通过 `ToolOverrideHandler` 统一分发，
//! core 不再硬编码特判这 5 个工具。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use serde_json::json;
use tiangong_core::core::Plugin;
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::session::Session;
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};

use crate::handler::{TaskStatus, task_registry, wait_tasks};

pub struct TaskPlugin {
    /// 各插件贡献的汇总环境变量（由 core 经 set_exec_env 回注）。
    runtime_env: RwLock<BTreeMap<String, String>>,
    workspace: RwLock<Option<PathBuf>>,
}

impl Default for TaskPlugin {
    fn default() -> Self {
        Self {
            runtime_env: RwLock::new(BTreeMap::new()),
            workspace: RwLock::new(None),
        }
    }
}

impl TaskPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取当前环境变量快照（spawn_task 时注入子进程）。
    fn env_snapshot(&self) -> Vec<(String, String)> {
        self.runtime_env
            .read()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    /// 工具调用分发（原 runtime.rs::handle_background_task）。
    fn dispatch(&self, call: &ToolCall, _session: &Session) -> Option<ToolResult> {
        match call.name.as_str() {
            "spawn_task" => self.handle_spawn(call),
            "query_task" => self.handle_query(call),
            "list_tasks" => self.handle_list(),
            "cancel_task" => self.handle_cancel(call),
            "wait_tasks" => self.handle_wait(call),
            _ => None, // 不是后台任务工具
        }
    }

    fn handle_spawn(&self, call: &ToolCall) -> Option<ToolResult> {
        let name = call
            .arguments
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("task")
            .to_string();
        let cmd = call
            .arguments
            .get("cmd")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let args: Vec<String> = call
            .arguments
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let cwd = call
            .arguments
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(String::from);

        if cmd.is_empty() {
            return Some(ToolResult {
                ok: false,
                summary: "缺少 cmd 参数".to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            });
        }

        let env = self.env_snapshot();

        match task_registry().lock() {
            Ok(mut reg) => match reg.spawn(name, cmd, args, cwd, env) {
                Ok(task_id) => Some(ToolResult {
                    ok: true,
                    summary: format!("后台任务已启动，task_id={task_id}"),
                    stdout: json!({"task_id": task_id}).to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                }),
                Err(e) => Some(ToolResult {
                    ok: false,
                    summary: format!("启动后台任务失败：{e}"),
                    stdout: String::new(),
                    stderr: e,
                    exit_code: 1,
                    execution: None,
                }),
            },
            Err(e) => Some(ToolResult {
                ok: false,
                summary: format!("任务注册表锁失败：{e}"),
                stdout: String::new(),
                stderr: e.to_string(),
                exit_code: 1,
                execution: None,
            }),
        }
    }

    fn handle_query(&self, call: &ToolCall) -> Option<ToolResult> {
        let task_id = call
            .arguments
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match task_registry().lock() {
            Ok(mut reg) => match reg.query(task_id) {
                Some(info) => {
                    let status_text = match &info.status {
                        TaskStatus::Running => "running".to_string(),
                        TaskStatus::Completed { exit_code } => {
                            format!("completed (exit_code={exit_code})")
                        }
                        TaskStatus::Failed { error } => format!("failed: {error}"),
                        TaskStatus::Cancelled => "cancelled".to_string(),
                    };
                    Some(ToolResult {
                        ok: true,
                        summary: format!("任务 {} 状态：{}", info.name, status_text),
                        stdout: serde_json::to_string_pretty(&info).unwrap_or_default(),
                        stderr: String::new(),
                        exit_code: 0,
                        execution: None,
                    })
                }
                None => Some(ToolResult {
                    ok: false,
                    summary: format!("未找到任务：{task_id}"),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 1,
                    execution: None,
                }),
            },
            Err(e) => Some(ToolResult {
                ok: false,
                summary: e.to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            }),
        }
    }

    fn handle_list(&self) -> Option<ToolResult> {
        match task_registry().lock() {
            Ok(mut reg) => {
                let tasks = reg.list();
                Some(ToolResult {
                    ok: true,
                    summary: format!("{} 个后台任务", tasks.len()),
                    stdout: serde_json::to_string_pretty(&tasks).unwrap_or_default(),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            }
            Err(e) => Some(ToolResult {
                ok: false,
                summary: e.to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            }),
        }
    }

    fn handle_cancel(&self, call: &ToolCall) -> Option<ToolResult> {
        let task_id = call
            .arguments
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match task_registry().lock() {
            Ok(mut reg) => match reg.cancel(task_id) {
                Some(info) => Some(ToolResult {
                    ok: true,
                    summary: format!("任务 {} 已取消", info.name),
                    stdout: serde_json::to_string_pretty(&info).unwrap_or_default(),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                }),
                None => Some(ToolResult {
                    ok: false,
                    summary: format!("未找到任务：{task_id}"),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 1,
                    execution: None,
                }),
            },
            Err(e) => Some(ToolResult {
                ok: false,
                summary: e.to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            }),
        }
    }

    fn handle_wait(&self, call: &ToolCall) -> Option<ToolResult> {
        let task_ids: Vec<String> = call
            .arguments
            .get("task_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let timeout_ms = call
            .arguments
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        if task_ids.is_empty() {
            return Some(ToolResult {
                ok: false,
                summary: "缺少 task_ids 参数".to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            });
        }

        let requested = task_ids.len();
        let results = wait_tasks(task_ids, timeout_ms);

        // 结果数少于请求数：存在不认识的 task id，与 query_task/cancel_task 的
        // 「未找到任务」语义一致——直接判定失败，避免把不存在的 id 误报为成功。
        if results.len() < requested {
            let missing = requested - results.len();
            return Some(ToolResult {
                ok: false,
                summary: format!(
                    "{missing} 个任务未找到（共请求 {requested} 个，仅查到 {} 个）",
                    results.len()
                ),
                stdout: serde_json::to_string_pretty(&results).unwrap_or_default(),
                stderr: format!("{missing} task id(s) not found"),
                exit_code: 1,
                execution: None,
            });
        }

        let all_ok = results
            .iter()
            .all(|r| matches!(r.status, TaskStatus::Completed { exit_code } if exit_code == 0));
        let running_count = results
            .iter()
            .filter(|r| matches!(r.status, TaskStatus::Running))
            .count();
        let summary = if running_count > 0 {
            format!(
                "{} 个任务完成，{} 个仍在运行（超时）",
                results.len() - running_count,
                running_count
            )
        } else {
            format!("{} 个任务全部完成", results.len())
        };

        Some(ToolResult {
            ok: all_ok,
            summary,
            stdout: serde_json::to_string_pretty(&results).unwrap_or_default(),
            stderr: String::new(),
            exit_code: if all_ok { 0 } else { 1 },
            execution: None,
        })
    }
}

impl Plugin for TaskPlugin {
    fn id(&self) -> &str {
        "task"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(std::path::Path::to_path_buf);
        }
    }

    fn set_exec_env(&self, env: BTreeMap<String, String>) {
        // 接收 core 汇总后的全部插件环境变量，spawn_task 执行子进程时注入。
        if let Ok(mut guard) = self.runtime_env.write() {
            *guard = env;
        }
    }
}

impl ToolSpecProvider for TaskPlugin {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "spawn_task".to_string(),
                description: "在后台启动特殊命令。仅当用户明确要求后台、不阻塞、并行执行、持续运行、启动服务/监听，或需要让命令跨多轮继续运行时使用；普通命令、构建、检查、git、文件操作必须优先使用 run_shell 或 run_command。注意：后台任务的 stdout/stderr 在进程结束前不被消费，高输出任务（如 dev server、日志刷屏）可能因 OS pipe 缓冲区写满而阻塞，请将输出重定向到文件。".to_string(),
                input_schema: json!({"type":"object","properties":{"name":{"type":"string"},"cmd":{"type":"string"},"args":{"type":"array","items":{"type":"string"}},"cwd":{"type":"string"}},"required":["name","cmd"]}),
            },
            ToolSpec {
                name: "query_task".to_string(),
                description: "查询后台任务状态".to_string(),
                input_schema: json!({"type":"object","properties":{"task_id":{"type":"string"}},"required":["task_id"]}),
            },
            ToolSpec {
                name: "list_tasks".to_string(),
                description: "列出所有后台任务".to_string(),
                input_schema: json!({"type":"object","properties":{}}),
            },
            ToolSpec {
                name: "cancel_task".to_string(),
                description: "取消后台任务".to_string(),
                input_schema: json!({"type":"object","properties":{"task_id":{"type":"string"}},"required":["task_id"]}),
            },
            ToolSpec {
                name: "wait_tasks".to_string(),
                description: "等待已通过 spawn_task 启动的后台任务完成。仅用于已有后台任务，不用于执行普通命令。".to_string(),
                input_schema: json!({"type":"object","properties":{"task_ids":{"type":"array","items":{"type":"string"}},"timeout_ms":{"type":"integer"}},"required":["task_ids"]}),
            },
        ]
    }
}

impl ToolOverrideHandler for TaskPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        session: &mut Session,
        _actor_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let result = self.dispatch(call, session);
        Box::pin(async move { result })
    }
}

impl PromptSectionProvider for TaskPlugin {
    fn prompt_sections(&self) -> Vec<String> {
        // 原 core rules 第 9 条：后台任务使用边界。
        vec!["只有用户明确要求后台、不阻塞、并行、持续运行、启动服务/监听，或需要管理已有后台任务时，才使用 spawn_task / wait_tasks。".to_string()]
    }
}
