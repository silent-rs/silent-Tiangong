//! 定时任务工具覆盖处理器。
//!
//! 实现 [`ToolOverrideHandler`]，直接从 LLM 传入的命名参数 JSON（`call.arguments`）
//! 按 key 取参，彻底绕开旧的「位置参数数组」模式，避免参数顺序错位导致的 not found。
//!
//! 同时实现 [`ToolSpecProvider`]，向 Agent 注入 6 个独立的定时任务工具规格，
//! 让 core 完全不感知 scheduler 的工具定义。

use anyhow::Result;
use serde_json::{json, Value};
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tiangong_scheduler::model::{Job, JobRun, JobRunStatus, TriggerType, UpdateJobRequest};
use tiangong_scheduler::store::JobStore;

/// 工具名常量：每个操作对应一个独立工具，LLM 无需再传 action 字段。
pub const TOOL_CREATE_JOB: &str = "scheduler_create_job";
pub const TOOL_LIST_JOBS: &str = "scheduler_list_jobs";
pub const TOOL_UPDATE_JOB: &str = "scheduler_update_job";
pub const TOOL_DELETE_JOB: &str = "scheduler_delete_job";
pub const TOOL_TRIGGER_JOB: &str = "scheduler_trigger_job";
pub const TOOL_GET_JOB_RUNS: &str = "scheduler_get_job_runs";

/// 定时任务工具覆盖处理器：按工具名分发，命名参数取值。
#[derive(Clone, Default)]
pub struct SchedulerToolOverride {
    /// 可选的存储根目录，用于测试隔离。生产环境为 None，使用默认的 `~/.tiangong/scheduler`。
    store_base: Option<std::path::PathBuf>,
}

impl SchedulerToolOverride {
    pub fn new() -> Self {
        Self { store_base: None }
    }

    /// 指定存储根目录（测试用），返回新的 handler 实例。
    #[cfg(test)]
    pub(crate) fn with_store_base(store_base: std::path::PathBuf) -> Self {
        Self {
            store_base: Some(store_base),
        }
    }

    /// 打开 JobStore：生产用默认路径，测试用注入路径。
    fn open_store(&self) -> Result<JobStore> {
        match &self.store_base {
            Some(base) => JobStore::open_at(base.clone()),
            None => JobStore::open(),
        }
    }

    /// 主分发入口：按 `call.name` 路由到对应处理函数。
    ///
    /// 每个处理函数返回 `Some(ToolResult)` 表示已处理；返回 `None` 表示工具名不匹配，
    /// 交回默认逻辑（实际上所有注册的工具名都会命中，None 仅作防御）。
    fn dispatch(&self, call: &ToolCall) -> Option<ToolResult> {
        let result = match call.name.as_str() {
            TOOL_CREATE_JOB => self.handle_create_job(call),
            TOOL_LIST_JOBS => self.handle_list_jobs(),
            TOOL_UPDATE_JOB => self.handle_update_job(call),
            TOOL_DELETE_JOB => self.handle_delete_job(call),
            TOOL_TRIGGER_JOB => self.handle_trigger_job(call),
            TOOL_GET_JOB_RUNS => self.handle_get_job_runs(call),
            _ => return None,
        };
        Some(result)
    }

    // ── 各操作实现 ───────────────────────────────────────────────

    fn handle_create_job(&self, call: &ToolCall) -> ToolResult {
        let args = &call.arguments;

        // 必填字段：缺任一即返回参数错误（不进入 store）
        let Some(name) = get_str_field(args, "name") else {
            return param_error("缺少必填参数 name");
        };
        let Some(description) = get_str_field(args, "description") else {
            return param_error("缺少必填参数 description");
        };
        let Some(schedule) = get_str_field(args, "schedule") else {
            return param_error("缺少必填参数 schedule");
        };
        let Some(payload) = get_str_field(args, "payload") else {
            return param_error("缺少必填参数 payload");
        };

        // 可选字段：session_id 为空时透传 None，沿用「首次触发时延迟创建会话」机制
        let session_id = get_opt_str_field(args, "session_id");
        let enabled = get_bool_field(args, "enabled").unwrap_or(true);

        let store = match self.open_store() {
            Ok(s) => s,
            Err(e) => return io_error("打开任务存储", e),
        };

        let now = chrono::Local::now().naive_local().to_string();
        let job = Job {
            id: scru128::new().to_string(),
            name,
            description,
            trigger_type: TriggerType::Cron,
            schedule: Some(schedule),
            session_id,
            payload,
            enabled,
            created_at: now.clone(),
            updated_at: now,
        };

        if let Err(e) = store.insert_job(&job) {
            return io_error("写入任务", e);
        }

        let output = json!({
            "id": job.id,
            "name": job.name,
            "schedule": job.schedule,
            "session_id": job.session_id,
            "enabled": job.enabled,
        });

        ToolResult {
            ok: true,
            summary: format!("已创建定时任务：{}", job.name),
            stdout: serde_json::to_string_pretty(&output).unwrap_or_default(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    fn handle_list_jobs(&self) -> ToolResult {
        let store = match self.open_store() {
            Ok(s) => s,
            Err(e) => return io_error("打开任务存储", e),
        };

        let jobs = match store.list_jobs() {
            Ok(j) => j,
            Err(e) => return io_error("查询任务列表", e),
        };

        let summary: Vec<Value> = jobs
            .iter()
            .map(|j| {
                json!({
                    "id": j.id,
                    "name": j.name,
                    "schedule": j.schedule,
                    "enabled": j.enabled,
                    "session_id": j.session_id,
                })
            })
            .collect();

        ToolResult {
            ok: true,
            summary: format!("共 {} 个定时任务", jobs.len()),
            stdout: serde_json::to_string_pretty(&summary).unwrap_or_default(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    fn handle_update_job(&self, call: &ToolCall) -> ToolResult {
        let args = &call.arguments;

        let Some(id) = get_str_field(args, "id") else {
            return param_error("缺少必填参数 id");
        };

        // 所有可更新字段均为可选，按 key 独立读取（None 表示不更新该字段）。
        // 可清空字段（schedule / session_id）用三态语义：显式传空串表示清空原值，
        // 未传或 null 表示保持不变；必填语义字段（name/description/payload）仍用
        // 旧逻辑（空串等同不更新，避免误清空成非法空值）。
        let req = UpdateJobRequest {
            name: get_opt_str_field(args, "name"),
            description: get_opt_str_field(args, "description"),
            schedule: get_nullable_str_field(args, "schedule").map(|opt| opt.unwrap_or_default()),
            session_id: get_nullable_str_field(args, "session_id")
                .map(|opt| opt.unwrap_or_default()),
            payload: get_opt_str_field(args, "payload"),
            enabled: get_bool_field(args, "enabled"),
        };

        let store = match self.open_store() {
            Ok(s) => s,
            Err(e) => return io_error("打开任务存储", e),
        };

        let updated = match store.update_job(&id, &req) {
            Ok(b) => b,
            Err(e) => return io_error("更新任务", e),
        };
        if !updated {
            return not_found(&id);
        }

        let job = match store.get_job(&id) {
            Ok(Some(j)) => j,
            Ok(None) => return not_found(&id),
            Err(e) => return io_error("查询更新后的任务", e),
        };

        let output = json!({
            "id": job.id,
            "name": job.name,
            "schedule": job.schedule,
            "enabled": job.enabled,
        });

        ToolResult {
            ok: true,
            summary: format!("已更新定时任务：{}", job.name),
            stdout: serde_json::to_string_pretty(&output).unwrap_or_default(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    fn handle_delete_job(&self, call: &ToolCall) -> ToolResult {
        let args = &call.arguments;

        let Some(id) = get_str_field(args, "id") else {
            return param_error("缺少必填参数 id");
        };

        let store = match self.open_store() {
            Ok(s) => s,
            Err(e) => return io_error("打开任务存储", e),
        };

        let deleted = match store.delete_job(&id) {
            Ok(b) => b,
            Err(e) => return io_error("删除任务", e),
        };

        ToolResult {
            ok: deleted,
            summary: if deleted {
                format!("已删除定时任务：{id}")
            } else {
                format!("定时任务 '{id}' 不存在")
            },
            stdout: String::new(),
            stderr: String::new(),
            exit_code: if deleted { 0 } else { 1 },
            execution: None,
        }
    }

    fn handle_trigger_job(&self, call: &ToolCall) -> ToolResult {
        let args = &call.arguments;

        let Some(id) = get_str_field(args, "id") else {
            return param_error("缺少必填参数 id");
        };

        let store = match self.open_store() {
            Ok(s) => s,
            Err(e) => return io_error("打开任务存储", e),
        };

        let job = match store.get_job(&id) {
            Ok(Some(j)) => j,
            Ok(None) => return not_found(&id),
            Err(e) => return io_error("查询任务", e),
        };

        // 记录一次手动触发（Agent 发起）的执行记录。
        //
        // 说明：plugin handler 所在链路无法访问 SchedulerContext（运行时上下文，含发消息/建会话能力），
        // 真正执行 LLM 调用由 GUI/Server 的 `job_trigger`（Tauri command / API）通过 execute_job 完成。
        // 此处只补记一条 JobRun，让 scheduler_get_job_runs 能查到手动触发历史，避免「已标记触发」
        // 但执行历史为空的不一致。状态直接记为 Succeeded（已成功登记触发请求）。
        let now = chrono::Local::now().naive_local().to_string();
        let run = JobRun {
            id: scru128::new().to_string(),
            job_id: job.id.clone(),
            session_id: job.session_id.clone().unwrap_or_default(),
            status: JobRunStatus::Succeeded,
            started_at: now.clone(),
            finished_at: Some(now),
            result_summary: Some("Agent 手动触发（已登记，实际执行由调度器完成）".to_string()),
        };
        if let Err(e) = store.insert_job_run(&run) {
            return io_error("记录触发历史", e);
        }

        ToolResult {
            ok: true,
            summary: format!(
                "定时任务 '{}' 已标记为触发（实际执行需通过调度器或 API 触发）",
                job.name
            ),
            stdout: serde_json::to_string_pretty(&json!({
                "id": job.id,
                "name": job.name,
                "status": "trigger_requested"
            }))
            .unwrap_or_default(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }

    fn handle_get_job_runs(&self, call: &ToolCall) -> ToolResult {
        let args = &call.arguments;

        let Some(id) = get_str_field(args, "id") else {
            return param_error("缺少必填参数 id");
        };
        let limit = get_usize_field(args, "limit").unwrap_or(10);

        let store = match self.open_store() {
            Ok(s) => s,
            Err(e) => return io_error("打开任务存储", e),
        };

        // 先校验任务存在，避免未知 id 返回「0 条」与「任务存在但从未执行」混淆。
        match store.get_job(&id) {
            Ok(Some(_)) => {}
            Ok(None) => return not_found(&id),
            Err(e) => return io_error("查询任务", e),
        }

        let runs = match store.list_job_runs(&id, limit) {
            Ok(r) => r,
            Err(e) => return io_error("查询执行历史", e),
        };

        let summary: Vec<Value> = runs
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "status": r.status,
                    "started_at": r.started_at,
                    "finished_at": r.finished_at,
                    "result_summary": r.result_summary,
                })
            })
            .collect();

        ToolResult {
            ok: true,
            summary: format!("共 {} 条执行记录", runs.len()),
            stdout: serde_json::to_string_pretty(&summary).unwrap_or_default(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        }
    }
}

impl ToolSpecProvider for SchedulerToolOverride {
    /// 返回 6 个独立的定时任务工具规格。
    ///
    /// 每个操作一个工具名，LLM 无需再传 action 字段；参数全部命名化，schema 清晰。
    fn tool_specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: TOOL_CREATE_JOB.to_string(),
                description: "创建定时任务（Cron Job）。指定 Cron 表达式、任务内容后，调度器会按周期自动触发。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "任务名称" },
                        "description": { "type": "string", "description": "任务描述" },
                        "schedule": { "type": "string", "description": "Cron 表达式，如 '0 9 * * *' 表示每天 9 点" },
                        "payload": { "type": "string", "description": "触发时发送给 LLM 的任务描述" },
                        "session_id": { "type": "string", "description": "关联已有会话 ID（可选，不指定则首次触发时自动创建新会话）" },
                        "enabled": { "type": "boolean", "description": "是否启用，默认 true" }
                    },
                    "required": ["name", "description", "schedule", "payload"]
                }),
            },
            ToolSpec {
                name: TOOL_LIST_JOBS.to_string(),
                description: "列出所有定时任务。".to_string(),
                input_schema: json!({"type": "object", "properties": {}, "required": []}),
            },
            ToolSpec {
                name: TOOL_UPDATE_JOB.to_string(),
                description: "更新已有定时任务的字段（所有字段可选，仅更新传入的字段）。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "任务 ID" },
                        "name": { "type": "string", "description": "任务名称" },
                        "description": { "type": "string", "description": "任务描述" },
                        "schedule": { "type": "string", "description": "Cron 表达式" },
                        "payload": { "type": "string", "description": "触发时发送给 LLM 的任务描述" },
                        "session_id": { "type": "string", "description": "关联会话 ID" },
                        "enabled": { "type": "boolean", "description": "是否启用" }
                    },
                    "required": ["id"]
                }),
            },
            ToolSpec {
                name: TOOL_DELETE_JOB.to_string(),
                description: "按 ID 删除定时任务。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "任务 ID" }
                    },
                    "required": ["id"]
                }),
            },
            ToolSpec {
                name: TOOL_TRIGGER_JOB.to_string(),
                description: "按 ID 手动触发一次定时任务（标记为触发，实际执行由调度器执行）。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "任务 ID" }
                    },
                    "required": ["id"]
                }),
            },
            ToolSpec {
                name: TOOL_GET_JOB_RUNS.to_string(),
                description: "查询指定定时任务的最近执行历史。".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "任务 ID" },
                        "limit": { "type": "integer", "description": "返回记录数量，默认 10" }
                    },
                    "required": ["id"]
                }),
            },
        ]
    }
}

impl ToolOverrideHandler for SchedulerToolOverride {
    fn handle(
        &self,
        call: &ToolCall,
        _session_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ToolResult>> + Send>> {
        let result = self.dispatch(call);
        Box::pin(async move { result })
    }
}

// ── 命名参数取值辅助函数 ──────────────────────────────────────
//
// 所有参数直接从 `arguments` JSON 按 key 读取，彻底避免位置索引错位。

/// 取必填字符串字段。空串/缺失/类型不符均返回 None（由调用方决定如何报错）。
fn get_str_field(args: &Value, key: &str) -> Option<String> {
    args.get(key)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// 取可选字符串字段。空串/缺失/null 返回 None，表示「不提供该值」。
fn get_opt_str_field(args: &Value, key: &str) -> Option<String> {
    let v = args.get(key)?;
    if v.is_null() {
        return None;
    }
    v.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// 取可更新的字符串字段的三态语义（用于 update 类操作）。
///
/// - `None`：未传该 key 或显式为 null → **不更新**（保持原值）
/// - `Some(None)`：传了空串 `""` → **清空**该字段（置为空）
/// - `Some(Some(s))`：传了非空串 → 更新为新值 `s`
///
/// 说明：受 `UpdateJobRequest` 字段类型（`Option<String>`）限制，清空在 store 层
/// 实际写入的是空串而非 `None`。此处把「显式空串」映射为 `Some(String::new())`，
/// 让 store 把原值覆盖为空，实现「清空原值」的语义（store 层彻底支持 `None` 需另改类型）。
fn get_nullable_str_field(args: &Value, key: &str) -> Option<Option<String>> {
    let v = args.get(key)?;
    if v.is_null() {
        // 显式 null 也视为「不更新」，与「未传」一致（null 在 JSON 里通常表示忽略）
        return None;
    }
    let s = v.as_str()?;
    if s.is_empty() {
        Some(None) // 显式空串 → 清空
    } else {
        Some(Some(s.to_string()))
    }
}

/// 取布尔字段。缺失/类型不符返回 None。
fn get_bool_field(args: &Value, key: &str) -> Option<bool> {
    args.get(key)?.as_bool()
}

/// 取 usize 字段。支持整数或可解析为整数的字符串。
fn get_usize_field(args: &Value, key: &str) -> Option<usize> {
    let v = args.get(key)?;
    if let Some(n) = v.as_u64() {
        return Some(n as usize);
    }
    v.as_str().and_then(|s| s.parse().ok())
}

// ── 统一的错误结果构造 ────────────────────────────────────────

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

fn io_error(action: &str, e: anyhow::Error) -> ToolResult {
    let msg = format!("{action}失败：{e}");
    ToolResult {
        ok: false,
        summary: msg.clone(),
        stdout: String::new(),
        stderr: msg,
        exit_code: 1,
        execution: None,
    }
}

fn not_found(id: &str) -> ToolResult {
    let msg = format!("定时任务 '{id}' 不存在");
    ToolResult {
        ok: false,
        summary: msg.clone(),
        stdout: String::new(),
        stderr: msg,
        exit_code: 1,
        execution: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tiangong_core::model::ToolCall;

    /// 构造一个绑定到临时存储目录的 handler，避免污染真实的 `~/.tiangong/scheduler`。
    fn handler_in_tmp() -> (SchedulerToolOverride, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let handler = SchedulerToolOverride::with_store_base(dir.path().to_path_buf());
        (handler, dir)
    }

    /// 构造一个命名参数 ToolCall（模拟 LLM 生成的调用）。
    fn make_call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "test-call".to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    /// 先通过 create_job 建一个任务，返回其 id（供后续操作使用）。
    fn seed_job(handler: &SchedulerToolOverride, name: &str) -> String {
        let call = make_call(
            TOOL_CREATE_JOB,
            json!({
                "name": name,
                "description": "测试任务",
                "schedule": "0 */1 * * * *",
                "payload": "hello",
            }),
        );
        let result = handler.dispatch(&call).expect("create 应被处理");
        assert!(result.ok, "create 应成功：{}", result.summary);
        // 从 stdout 的 JSON 里提取 id
        let parsed: Value = serde_json::from_str(&result.stdout).expect("create 输出应为 JSON");
        parsed["id"].as_str().expect("create 输出含 id").to_string()
    }

    #[test]
    fn create_and_list_job() {
        let (handler, _dir) = handler_in_tmp();
        let id = seed_job(&handler, "任务A");

        // list 应包含刚创建的任务
        let call = make_call(TOOL_LIST_JOBS, json!({}));
        let result = handler.dispatch(&call).expect("list 应被处理");
        assert!(result.ok);
        assert!(
            result.summary.contains('1'),
            "应有 1 个任务：{}",
            result.summary
        );

        // 再次 create 后 list 应有 2 个
        seed_job(&handler, "任务B");
        let result = handler
            .dispatch(&make_call(TOOL_LIST_JOBS, json!({})))
            .unwrap();
        assert!(result.summary.contains('2'));
        let _ = id;
    }

    /// 核心回归测试：delete 必须按 id 命中（旧实现因位置错位用空串查不到）。
    #[test]
    fn delete_job_by_id_succeeds() {
        let (handler, _dir) = handler_in_tmp();
        let id = seed_job(&handler, "待删除");

        let call = make_call(TOOL_DELETE_JOB, json!({ "id": id }));
        let result = handler.dispatch(&call).expect("delete 应被处理");
        assert!(result.ok, "删除应成功：{}", result.summary);
        assert_eq!(result.exit_code, 0);

        // 删除后 list 应为空
        let list = handler
            .dispatch(&make_call(TOOL_LIST_JOBS, json!({})))
            .unwrap();
        assert!(list.summary.contains('0'));
    }

    /// 核心回归测试：delete 不存在的 id 返回失败（而非用空串误删）。
    #[test]
    fn delete_nonexistent_job_reports_not_found() {
        let (handler, _dir) = handler_in_tmp();
        let call = make_call(TOOL_DELETE_JOB, json!({ "id": "不存在的id" }));
        let result = handler.dispatch(&call).unwrap();
        assert!(!result.ok);
        assert_eq!(result.exit_code, 1);
        assert!(result.summary.contains("不存在"));
    }

    /// 核心回归测试：trigger 必须按 id 命中（旧实现因位置错位用空串查不到）。
    #[test]
    fn trigger_job_by_id_succeeds() {
        let (handler, _dir) = handler_in_tmp();
        let id = seed_job(&handler, "待触发");

        let call = make_call(TOOL_TRIGGER_JOB, json!({ "id": id }));
        let result = handler.dispatch(&call).expect("trigger 应被处理");
        assert!(result.ok, "触发应成功：{}", result.summary);
        assert!(result.summary.contains("待触发"));
    }

    /// 核心回归测试：update 必须按 id 命中并更新字段（旧实现因位置错位用空串报 not found）。
    #[test]
    fn update_job_by_id_succeeds() {
        let (handler, _dir) = handler_in_tmp();
        let id = seed_job(&handler, "原名");

        let call = make_call(
            TOOL_UPDATE_JOB,
            json!({ "id": id, "name": "新名", "enabled": false }),
        );
        let result = handler.dispatch(&call).expect("update 应被处理");
        assert!(result.ok, "更新应成功：{}", result.summary);

        // 通过 list 验证字段已更新
        let list = handler
            .dispatch(&make_call(TOOL_LIST_JOBS, json!({})))
            .unwrap();
        let parsed: Vec<Value> = serde_json::from_str(&list.stdout).unwrap();
        let job = parsed.iter().find(|j| j["id"] == id).unwrap();
        assert_eq!(job["name"], "新名");
        assert_eq!(job["enabled"], false);
    }

    /// update 不存在的 id 应报 not found。
    #[test]
    fn update_nonexistent_job_reports_not_found() {
        let (handler, _dir) = handler_in_tmp();
        let call = make_call(TOOL_UPDATE_JOB, json!({ "id": "不存在", "name": "新名" }));
        let result = handler.dispatch(&call).unwrap();
        assert!(!result.ok);
        assert!(result.summary.contains("不存在"));
    }

    /// get_job_runs 必须按 id 命中。
    #[test]
    fn get_job_runs_by_id() {
        let (handler, _dir) = handler_in_tmp();
        let id = seed_job(&handler, "查历史");

        // 新任务无执行记录，应返回 0 条而非报错
        let call = make_call(TOOL_GET_JOB_RUNS, json!({ "id": id }));
        let result = handler.dispatch(&call).expect("get_job_runs 应被处理");
        assert!(result.ok);
        assert!(result.summary.contains('0'));
    }

    /// 缺少必填参数 id 时应返回参数错误（而非继续访问 store）。
    #[test]
    fn delete_without_id_returns_param_error() {
        let (handler, _dir) = handler_in_tmp();
        let call = make_call(TOOL_DELETE_JOB, json!({}));
        let result = handler.dispatch(&call).unwrap();
        assert!(!result.ok);
        assert!(result.summary.contains("id"));
    }

    /// session_id 为空（不传）时 create 应成功，job 的 session_id 为 null。
    #[test]
    fn create_job_without_session_id() {
        let (handler, _dir) = handler_in_tmp();
        let call = make_call(
            TOOL_CREATE_JOB,
            json!({
                "name": "无会话任务",
                "description": "验证 session_id 可选",
                "schedule": "0 9 * * *",
                "payload": "ping",
            }),
        );
        let result = handler.dispatch(&call).unwrap();
        assert!(result.ok);
        let parsed: Value = serde_json::from_str(&result.stdout).unwrap();
        assert!(
            parsed["session_id"].is_null(),
            "未传 session_id 时应为 null"
        );
    }

    /// 未注册的工具名应返回 None（交回默认逻辑）。
    #[test]
    fn unknown_tool_name_returns_none() {
        let (handler, _dir) = handler_in_tmp();
        let call = make_call("not_a_scheduler_tool", json!({}));
        assert!(handler.dispatch(&call).is_none());
    }

    // ── 缺陷回归保护（Review #1/#3/#4，已修复）─────────────────────────
    //
    // 以下三个测试原为暴露缺陷的失败基线（#[ignore]），对应生产代码已修复，
    // 现作为永久回归保护运行。
    //
    //   - trigger_job_should_record_run            → Review 第 1 项：手动触发补记执行历史
    //   - get_job_runs_unknown_job_should_error    → Review 第 3 项：校验任务存在
    //   - update_job_can_clear_optional_field      → Review 第 4 项：显式空串清空字段

    /// 打开与 handler 同一存储根的 JobStore，供测试断言副作用（执行历史等）。
    fn store_at(dir: &tempfile::TempDir) -> tiangong_scheduler::store::JobStore {
        tiangong_scheduler::store::JobStore::open_at(dir.path().to_path_buf()).unwrap()
    }

    /// Review 第 1 项：`scheduler_trigger_job` 原先只返回「已标记触发」提示，不写任何
    /// 执行历史，导致 scheduler_get_job_runs 查不到手动触发记录。修复后应补记一条
    /// JobRun（状态 Succeeded）。
    ///
    /// 说明：plugin handler 链路无法访问 SchedulerContext，无法真正执行 LLM 调用——
    /// 那由 GUI/Server 的 job_trigger（execute_job）完成。此处仅校验「补记」行为。
    #[test]
    fn trigger_job_should_record_run() {
        let (handler, dir) = handler_in_tmp();
        let id = seed_job(&handler, "待触发");

        let call = make_call(TOOL_TRIGGER_JOB, json!({ "id": id }));
        let result = handler.dispatch(&call).expect("trigger 应被处理");
        assert!(result.ok, "触发应成功：{}", result.summary);

        // 触发后应补记至少一条执行记录
        let store = store_at(&dir);
        let runs = store.list_job_runs(&id, 10).unwrap();
        assert!(
            !runs.is_empty(),
            "手动触发应产生执行记录，当前 {} 条",
            runs.len()
        );
    }

    /// Review 第 3 项：`scheduler_get_job_runs` 原先不校验任务存在，未知 id 返回
    /// ok=true + 「0 条记录」，与「任务存在但从未执行」混淆。修复后未知任务应明确报错
    /// （ok=false，summary 含「不存在」）。
    #[test]
    fn get_job_runs_unknown_job_should_error() {
        let (handler, _dir) = handler_in_tmp();
        let call = make_call(TOOL_GET_JOB_RUNS, json!({ "id": "不存在的任务id" }));
        let result = handler.dispatch(&call).expect("get_job_runs 应被处理");

        assert!(
            !result.ok,
            "未知任务应报错，当前却返回成功：{}",
            result.summary
        );
        assert!(result.summary.contains("不存在"));
    }

    /// Review 第 4 项：`scheduler_update_job` 原先把空串/null 都当作「不更新」，
    /// 无法清空可选字段。修复后显式传空串应清空原值。
    ///
    /// 注意：受 store 层 `UpdateJobRequest.session_id: Option<String>` 类型限制，
    /// 清空在存储中体现为 `Some("")`（空串覆盖原值），而非 `None`。彻底支持 `None`
    /// 需把 store 字段改为 `Option<Option<String>>`，超出本次 handler 层修复范围。
    /// 此处断言「原值已被覆盖为空」，即不再是创建时的 "sess-original"。
    #[test]
    fn update_job_can_clear_optional_field() {
        let (handler, dir) = handler_in_tmp();

        // 先创建一个带 session_id 的任务
        let call = make_call(
            TOOL_CREATE_JOB,
            json!({
                "name": "带会话",
                "description": "验证清空字段",
                "schedule": "0 9 * * *",
                "payload": "ping",
                "session_id": "sess-original",
            }),
        );
        let result = handler.dispatch(&call).expect("create 应被处理");
        assert!(result.ok, "{}", result.summary);
        let id = {
            let parsed: Value = serde_json::from_str(&result.stdout).unwrap();
            parsed["id"].as_str().unwrap().to_string()
        };

        // 显式传空串，意图清空 session_id
        let call = make_call(TOOL_UPDATE_JOB, json!({ "id": id, "session_id": "" }));
        let result = handler.dispatch(&call).expect("update 应被处理");
        assert!(result.ok, "{}", result.summary);

        // 原值 "sess-original" 应已被清空覆盖（store 层写作 Some("")，非 None）。
        let job = store_at(&dir).get_job(&id).unwrap().unwrap();
        assert_ne!(
            job.session_id.as_deref(),
            Some("sess-original"),
            "显式传空串应清空原 session_id，实际为 {:?}",
            job.session_id
        );
        assert!(
            job.session_id.as_deref().map_or(true, |s| s.is_empty()),
            "清空后 session_id 应为空串（store 层限制），实际为 {:?}",
            job.session_id
        );
    }
}
