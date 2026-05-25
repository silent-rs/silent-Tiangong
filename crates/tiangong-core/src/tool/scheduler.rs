use anyhow::{Context, Result};
use serde_json::json;

use super::{LocalToolExecutor, ToolCall, ToolResult};
use crate::scheduler::model::{Job, TriggerType, UpdateJobRequest};
use crate::scheduler::store::JobStore;

impl LocalToolExecutor {
    pub(super) fn scheduler(&self, call: &ToolCall) -> Result<ToolResult> {
        let action = call.args.first().map(|s| s.as_str()).unwrap_or("");

        match action {
            "create_job" => self.scheduler_create_job(call),
            "list_jobs" => self.scheduler_list_jobs(),
            "update_job" => self.scheduler_update_job(call),
            "delete_job" => self.scheduler_delete_job(call),
            "trigger_job" => self.scheduler_trigger_job(call),
            "get_job_runs" => self.scheduler_get_job_runs(call),
            _ => Err(anyhow::anyhow!(
                "未知操作：{action}。支持：create_job, list_jobs, update_job, delete_job, trigger_job, get_job_runs"
            )),
        }
    }

    fn scheduler_create_job(&self, call: &ToolCall) -> Result<ToolResult> {
        let name = call.args.get(1).context("缺少 name 参数")?;
        let description = call.args.get(2).context("缺少 description 参数")?;
        let schedule = call.args.get(3).context("缺少 schedule 参数")?;
        let payload = call.args.get(4).context("缺少 payload 参数")?;
        let session_id = call.args.get(5).and_then(|s| {
            if s.is_empty() || s == "null" {
                None
            } else {
                Some(s.clone())
            }
        });

        let store = JobStore::open()?;
        let now = chrono::Local::now().naive_local().to_string();
        let job = Job {
            id: scru128::new().to_string(),
            name: name.clone(),
            description: description.clone(),
            trigger_type: TriggerType::Cron,
            schedule: Some(schedule.clone()),
            session_id,
            payload: payload.clone(),
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
        };

        store.insert_job(&job)?;

        let output = json!({
            "id": job.id,
            "name": job.name,
            "schedule": job.schedule,
            "enabled": job.enabled,
        });

        Ok(ToolResult {
            ok: true,
            summary: format!("已创建定时任务：{}", job.name),
            stdout: serde_json::to_string_pretty(&output)?,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    fn scheduler_list_jobs(&self) -> Result<ToolResult> {
        let store = JobStore::open()?;
        let jobs = store.list_jobs()?;
        let summary: Vec<serde_json::Value> = jobs
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

        Ok(ToolResult {
            ok: true,
            summary: format!("共 {} 个定时任务", jobs.len()),
            stdout: serde_json::to_string_pretty(&summary)?,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    fn scheduler_update_job(&self, call: &ToolCall) -> Result<ToolResult> {
        let id = call.args.get(1).context("缺少 id 参数")?;
        let req = UpdateJobRequest {
            name: call.args.get(2).and_then(|v| {
                if v.is_empty() || v == "null" {
                    None
                } else {
                    Some(v.clone())
                }
            }),
            description: call.args.get(3).and_then(|v| {
                if v.is_empty() || v == "null" {
                    None
                } else {
                    Some(v.clone())
                }
            }),
            schedule: call.args.get(4).and_then(|v| {
                if v.is_empty() || v == "null" {
                    None
                } else {
                    Some(v.clone())
                }
            }),
            session_id: call.args.get(5).and_then(|v| {
                if v.is_empty() || v == "null" {
                    None
                } else {
                    Some(v.clone())
                }
            }),
            payload: call.args.get(6).and_then(|v| {
                if v.is_empty() || v == "null" {
                    None
                } else {
                    Some(v.clone())
                }
            }),
            enabled: call.args.get(7).and_then(|v| v.parse().ok()),
        };

        let store = JobStore::open()?;
        let updated = store.update_job(id, &req)?;
        if !updated {
            return Ok(ToolResult {
                ok: false,
                summary: format!("定时任务 '{id}' 不存在"),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            });
        }

        let job = store.get_job(id)?.context("更新后查询失败")?;
        let output = json!({
            "id": job.id,
            "name": job.name,
            "schedule": job.schedule,
            "enabled": job.enabled,
        });

        Ok(ToolResult {
            ok: true,
            summary: format!("已更新定时任务：{}", job.name),
            stdout: serde_json::to_string_pretty(&output)?,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }

    fn scheduler_delete_job(&self, call: &ToolCall) -> Result<ToolResult> {
        let id = call.args.get(1).context("缺少 id 参数")?;
        let store = JobStore::open()?;
        let deleted = store.delete_job(id)?;
        Ok(ToolResult {
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
        })
    }

    fn scheduler_trigger_job(&self, call: &ToolCall) -> Result<ToolResult> {
        let id = call.args.get(1).context("缺少 id 参数")?;
        let store = JobStore::open()?;
        let job = store.get_job(id)?;
        match job {
            Some(j) => Ok(ToolResult {
                ok: true,
                summary: format!(
                    "定时任务 '{}' 已标记为触发（实际执行需在 Server 模式下通过 API 触发）",
                    j.name
                ),
                stdout: serde_json::to_string_pretty(&json!({
                    "id": j.id,
                    "name": j.name,
                    "status": "trigger_requested"
                }))?,
                stderr: String::new(),
                exit_code: 0,
                execution: None,
            }),
            None => Ok(ToolResult {
                ok: false,
                summary: format!("定时任务 '{id}' 不存在"),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 1,
                execution: None,
            }),
        }
    }

    fn scheduler_get_job_runs(&self, call: &ToolCall) -> Result<ToolResult> {
        let id = call.args.get(1).context("缺少 id 参数")?;
        let limit: usize = call.args.get(2).and_then(|v| v.parse().ok()).unwrap_or(10);

        let store = JobStore::open()?;
        let runs = store.list_job_runs(id, limit)?;
        let summary: Vec<serde_json::Value> = runs
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

        Ok(ToolResult {
            ok: true,
            summary: format!("共 {} 条执行记录", runs.len()),
            stdout: serde_json::to_string_pretty(&summary)?,
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}
