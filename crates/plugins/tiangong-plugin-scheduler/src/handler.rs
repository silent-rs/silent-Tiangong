//! 定时任务工具规格与覆盖处理器实现。
//!
//! 实现 [`ToolSpecProvider`] 与 [`ToolOverrideHandler`]，直接从 LLM 传入的命名参数
//! JSON（`call.arguments`）按 key 取参，彻底绕开旧的「位置参数数组」模式，避免参数
//! 顺序错位导致的 not found。
//!
//! 向 Agent 注入 6 个独立的定时任务工具规格，让 core 完全不感知 scheduler 的工具定义。

use anyhow::Result;
use serde_json::{json, Value};
use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::ToolResult;
use tiangong_core::tool_override::{ToolOverrideHandler, ToolSpecProvider};
use tiangong_scheduler::model::{Job, TriggerType, UpdateJobRequest};
use tiangong_scheduler::store::JobStore;

use crate::plugin::SchedulerPlugin;

/// 工具名常量：每个操作对应一个独立工具，LLM 无需再传 action 字段。
pub const TOOL_CREATE_JOB: &str = "scheduler_create_job";
pub const TOOL_LIST_JOBS: &str = "scheduler_list_jobs";
pub const TOOL_UPDATE_JOB: &str = "scheduler_update_job";
pub const TOOL_DELETE_JOB: &str = "scheduler_delete_job";
pub const TOOL_TRIGGER_JOB: &str = "scheduler_trigger_job";
pub const TOOL_GET_JOB_RUNS: &str = "scheduler_get_job_runs";

impl SchedulerPlugin {
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
    pub(crate) fn dispatch(&self, call: &ToolCall) -> Option<ToolResult> {
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

        // 校验 cron 表达式：调度器要求 6 字段（秒 分 时 日 月 周），否则恢复期会解析失败。
        // 在此提前拦截，避免无效表达式静默落库后开机才报错。
        if let Err(e) = tiangong_scheduler::executor::validate_cron_schedule(&schedule) {
            return param_error(&format!(
                "schedule 不是合法的 cron 表达式（需 6 字段，如 '0 25 21 * * *' 表示每天 21:25）：{e}"
            ));
        }

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

        let job = match store.insert_job(&job) {
            Ok(job) => job,
            Err(e) => return io_error("写入任务", e),
        };

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

        // 更新 schedule 时同样校验（三态：未传/None 表示不变，显式空串表示清空，
        // 非空串才需校验）。与 create 用同一校验，确保落库即可解析。
        if let Some(schedule) = req.schedule.as_ref().filter(|s| !s.is_empty()) {
            if let Err(e) = tiangong_scheduler::executor::validate_cron_schedule(schedule) {
                return param_error(&format!(
                    "schedule 不是合法的 cron 表达式（需 6 字段，如 '0 25 21 * * *' 表示每天 21:25）：{e}"
                ));
            }
        }

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

        // 交给 execute_job_with_store 真正执行：解析/新建任务专属会话、写
        // Running→Succeeded/Failed 的真实 JobRun、把 `[定时任务触发]` 消息经宿主路由
        // 投递给 Core。行为与 UI 按钮（job_trigger）和 cron 调度完全一致。异步执行，
        // 不阻塞当前 Agent turn。context 由入口层必填注入，无需降级处理。
        let ctx = self.context.clone();
        let job_clone = job.clone();
        let store_clone = store.clone();
        tokio::spawn(async move {
            tracing::info!(job_id = %job_clone.id, "Agent 手动触发定时任务，开始执行");
            tiangong_scheduler::executor::execute_job_with_store(ctx, job_clone, store_clone).await;
        });

        ToolResult {
            ok: true,
            summary: format!(
                "定时任务 '{}' 已触发并开始执行（异步，结果见执行历史）",
                job.name
            ),
            stdout: serde_json::to_string_pretty(&json!({
                "id": job.id,
                "name": job.name,
                "status": "triggered"
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

impl ToolSpecProvider for SchedulerPlugin {
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
                        "schedule": { "type": "string", "description": "Cron 表达式（6 字段：秒 分 时 日 月 周），如 '0 0 9 * * *' 表示每天 9 点" },
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
                        "schedule": { "type": "string", "description": "Cron 表达式（6 字段：秒 分 时 日 月 周）" },
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
                description: "按 ID 立即手动触发一次定时任务执行（会真正执行任务并把结果写入执行历史）。".to_string(),
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

impl ToolOverrideHandler for SchedulerPlugin {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &mut tiangong_core::session::Session,
        _actor_id: &str,
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
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;
    use tiangong_core::model::ToolCall;
    use tiangong_scheduler::executor::SchedulerContext;
    use tiangong_scheduler::model::JobRunStatus;

    /// 记录消息投递次数的 mock 执行上下文。
    ///
    /// 复用 execute_job_with_store 真实链路（resolve_session_id + send_message），
    /// 通过 send_count 断言「任务确实被执行」，而非只登记。
    #[derive(Default)]
    struct RecordingContext {
        send_count: StdArc<AtomicUsize>,
    }

    #[async_trait]
    impl SchedulerContext for RecordingContext {
        async fn send_message(&self, _session_id: &str, _content: String) -> Result<()> {
            self.send_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn resolve_session_id(
            &self,
            requested_session_id: Option<&str>,
        ) -> Result<(String, bool)> {
            if let Some(sid) = requested_session_id {
                return Ok((sid.to_string(), false));
            }
            Ok((scru128::new().to_string(), true))
        }
    }

    /// 构造一个绑定到临时存储目录、注入 RecordingContext 的插件。
    ///
    /// context 为必填项，测试统一注入 RecordingContext，避免污染真实的
    /// `~/.tiangong/scheduler`。需要断言 execute_job 是否被调用时，用
    /// [`handler_in_tmp_with_send_count`] 取回计数器。
    fn handler_in_tmp() -> (SchedulerPlugin, tempfile::TempDir) {
        handler_in_tmp_with_send_count(StdArc::new(AtomicUsize::new(0)))
    }

    /// 同 [`handler_in_tmp`]，但返回 RecordingContext 的 send_count 供测试轮询。
    fn handler_in_tmp_with_send_count(
        send_count: StdArc<AtomicUsize>,
    ) -> (SchedulerPlugin, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let ctx: StdArc<dyn SchedulerContext> = StdArc::new(RecordingContext { send_count });
        let handler = SchedulerPlugin::new(ctx).with_store_base(dir.path().to_path_buf());
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
    fn seed_job(handler: &SchedulerPlugin, name: &str) -> String {
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
                "schedule": "0 0 9 * * *",
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

    // ── cron 表达式校验（5 字段被拒、6 字段通过）──────────────────────

    /// 5 字段 crontab 写法（如 `25 21 * * *`）应被 create 拒绝：调度器底层 cron
    /// crate 要求 6 字段（秒 分 时 日 月 周），5 字段会在第 6 字段处 EOF 失败。
    /// 回归保护：在线日志曾出现 `解析 cron 表达式失败 [25 21 * * *]`。
    #[test]
    fn create_job_rejects_five_field_cron() {
        let (handler, _dir) = handler_in_tmp();
        let call = make_call(
            TOOL_CREATE_JOB,
            json!({
                "name": "五字段任务",
                "description": "应被拒绝",
                "schedule": "25 21 * * *",
                "payload": "ping",
            }),
        );
        let result = handler.dispatch(&call).expect("create 应被处理");
        assert!(
            !result.ok,
            "5 字段 cron 应被拒绝，却成功了：{}",
            result.summary
        );
        assert!(
            result.summary.contains("cron") || result.stdout.contains("cron"),
            "错误信息应说明是 cron 校验失败：{}",
            result.summary
        );
    }

    /// 等价的 6 字段写法（`0 25 21 * * *`，每天 21:25）应创建成功。
    #[test]
    fn create_job_accepts_six_field_cron() {
        let (handler, _dir) = handler_in_tmp();
        let call = make_call(
            TOOL_CREATE_JOB,
            json!({
                "name": "六字段任务",
                "description": "应通过",
                "schedule": "0 25 21 * * *",
                "payload": "ping",
            }),
        );
        let result = handler.dispatch(&call).expect("create 应被处理");
        assert!(result.ok, "6 字段 cron 应通过：{}", result.summary);
    }

    /// update 同样校验：传入 5 字段应被拒。
    #[test]
    fn update_job_rejects_five_field_cron() {
        let (handler, _dir) = handler_in_tmp();
        let id = seed_job(&handler, "原任务");
        let call = make_call(
            TOOL_UPDATE_JOB,
            json!({ "id": id, "schedule": "0 9 * * *" }),
        );
        let result = handler.dispatch(&call).expect("update 应被处理");
        assert!(!result.ok, "5 字段 cron 更新应被拒绝：{}", result.summary);
    }

    // ── 缺陷回归保护（Review #1/#3/#4，已修复）─────────────────────────
    //
    // 以下测试为永久回归保护：
    //
    //   - trigger_job_actually_executes            → 手动触发真正执行（execute_job + send_message）
    //   - get_job_runs_unknown_job_should_error    → Review 第 3 项：校验任务存在
    //   - update_job_can_clear_optional_field      → Review 第 4 项：显式空串清空字段

    /// 打开与插件同一存储根的 JobStore，供测试断言副作用（执行历史等）。
    fn store_at(dir: &tempfile::TempDir) -> tiangong_scheduler::store::JobStore {
        tiangong_scheduler::store::JobStore::open_at(dir.path().to_path_buf()).unwrap()
    }

    /// 核心回归：Agent 手动触发应真正执行任务（调用 `execute_job_with_store` →
    /// `send_message`），而非只登记。
    ///
    /// 修复前 handler 只写假 Succeeded 记录（started_at == finished_at）就返回，
    /// 从不调用 execute_job，是本次 Bug 的直接现场。现在 context 必填注入，触发即执行。
    ///
    /// 断言三件事：
    /// 1. handler 立即返回 ok=true（已派发执行）；
    /// 2. 后台 send_message 被真正调用（证明 execute_job 跑通）；
    /// 3. JobRun 为 Succeeded 且 started_at != finished_at（真实执行，非假记录）。
    #[tokio::test]
    async fn trigger_job_actually_executes() {
        let send_count = StdArc::new(AtomicUsize::new(0));
        let (handler, dir) = handler_in_tmp_with_send_count(send_count.clone());
        let id = seed_job(&handler, "待触发");

        let call = make_call(TOOL_TRIGGER_JOB, json!({ "id": id }));
        let result = handler.dispatch(&call).expect("trigger 应被处理");
        assert!(result.ok, "注入上下文后触发应成功派发：{}", result.summary);

        // execute_job 在后台 tokio::spawn 中异步执行，轮询等待 send_message 被调用。
        let waited = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            poll_until(send_count.clone(), 1),
        )
        .await;
        assert!(
            waited.is_ok(),
            "execute_job 应在后台调用 send_message，但 3s 内未触发"
        );

        // 真实执行应写一条 Succeeded 记录，且起止时间不同（execute_core 各写一次 now）。
        let store = store_at(&dir);
        let runs = store.list_job_runs(&id, 10).unwrap();
        let succeeded = runs
            .iter()
            .find(|r| matches!(r.status, JobRunStatus::Succeeded))
            .expect("应存在一条 Succeeded 记录");
        assert_ne!(
            succeeded.started_at,
            succeeded.finished_at.clone().unwrap_or_default(),
            "真实执行的起止时间应不同（修复前假记录两者相同）"
        );
    }

    /// 轮询直到 send_count 达到 target，用于异步等待后台 execute_job 完成。
    async fn poll_until(counter: StdArc<AtomicUsize>, target: usize) {
        loop {
            if counter.load(Ordering::SeqCst) >= target {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
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
                "schedule": "0 0 9 * * *",
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
            job.session_id.as_deref().is_none_or(|s| s.is_empty()),
            "清空后 session_id 应为空串（store 层限制），实际为 {:?}",
            job.session_id
        );
    }
}
