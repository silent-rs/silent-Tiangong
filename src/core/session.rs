use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::core::planner::{PlanStep, PlanStepStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    #[serde(default)]
    pub reasoning_content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub task_records: Vec<SessionTaskRecord>,
    #[serde(default)]
    pub plan_steps: Vec<SessionPlanStep>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionTaskStatus {
    Planning,
    Executing,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTaskRecord {
    pub task_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub user_input: String,
    pub status: SessionTaskStatus,
    pub summary: String,
    #[serde(default)]
    pub plan_snapshot: Option<String>,
    #[serde(default)]
    pub tool_result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPlanStep {
    pub id: String,
    pub task_id: String,
    pub name: String,
    pub description: String,
    pub status: PlanStepStatus,
    pub created_at: String,
    pub updated_at: String,
}

impl Session {
    pub fn new(title: impl Into<String>) -> Self {
        let now = now_text();
        Self {
            id: new_id(),
            title: title.into(),
            messages: Vec::new(),
            task_records: Vec::new(),
            plan_steps: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn append_message(&mut self, role: MessageRole, content: impl Into<String>) {
        self.append_message_with_reasoning(role, content, String::new());
    }

    pub fn append_message_with_reasoning(
        &mut self,
        role: MessageRole,
        content: impl Into<String>,
        reasoning_content: impl Into<String>,
    ) {
        self.messages.push(Message {
            id: new_id(),
            role,
            content: content.into(),
            reasoning_content: reasoning_content.into(),
            created_at: now_text(),
        });
        self.updated_at = now_text();
    }

    pub fn start_task(
        &mut self,
        task_id: String,
        user_message_id: String,
        assistant_message_id: String,
        user_input: String,
    ) {
        let now = now_text();
        self.task_records.push(SessionTaskRecord {
            task_id,
            user_message_id,
            assistant_message_id,
            user_input,
            status: SessionTaskStatus::Planning,
            summary: "正在生成执行计划".to_string(),
            plan_snapshot: None,
            tool_result: None,
            error: None,
            started_at: now.clone(),
            updated_at: now,
            finished_at: None,
            duration_ms: None,
        });
        self.updated_at = now_text();
    }

    pub fn mark_task_executing(&mut self, task_id: &str, plan_snapshot: Option<String>) {
        let Some(record) = self
            .task_records
            .iter_mut()
            .find(|record| record.task_id == task_id)
        else {
            return;
        };
        record.status = SessionTaskStatus::Executing;
        record.summary = "正在执行任务".to_string();
        if let Some(plan_snapshot) = plan_snapshot {
            record.plan_snapshot = Some(plan_snapshot);
        }
        record.updated_at = now_text();
        self.updated_at = now_text();
    }

    pub fn sync_task_plan_steps(&mut self, task_id: &str, steps: &[PlanStep]) {
        for step in steps {
            if let Some(existing) = self
                .plan_steps
                .iter_mut()
                .find(|record| record.task_id == task_id && record.id == step.id)
            {
                existing.name = step.name.clone();
                existing.description = step.description.clone();
                existing.status = step.status;
                existing.updated_at = now_text();
                continue;
            }

            let now = now_text();
            self.plan_steps.push(SessionPlanStep {
                id: step.id.clone(),
                task_id: task_id.to_string(),
                name: step.name.clone(),
                description: step.description.clone(),
                status: step.status,
                created_at: now.clone(),
                updated_at: now,
            });
        }
        self.updated_at = now_text();
    }

    pub fn delete_pending_plan_step(&mut self, pending_index: usize) -> bool {
        let Some(pos) = self
            .pending_plan_step_positions()
            .get(pending_index)
            .copied()
        else {
            return false;
        };
        self.plan_steps.remove(pos);
        self.updated_at = now_text();
        true
    }

    pub fn move_pending_plan_step(&mut self, from_idx: usize, to_idx: usize) -> bool {
        let pending_positions = self.pending_plan_step_positions();
        if pending_positions.is_empty()
            || from_idx >= pending_positions.len()
            || to_idx >= pending_positions.len()
            || from_idx == to_idx
        {
            return false;
        }

        let mut pending = pending_positions
            .iter()
            .map(|idx| self.plan_steps[*idx].clone())
            .collect::<Vec<_>>();
        let item = pending.remove(from_idx);
        pending.insert(to_idx, item);

        for (slot, item) in pending_positions.iter().zip(pending.into_iter()) {
            self.plan_steps[*slot] = item;
        }
        self.updated_at = now_text();
        true
    }

    fn pending_plan_step_positions(&self) -> Vec<usize> {
        self.plan_steps
            .iter()
            .enumerate()
            .filter_map(|(idx, step)| (step.status == PlanStepStatus::Pending).then_some(idx))
            .collect()
    }

    pub fn complete_task(
        &mut self,
        task_id: &str,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
        duration_ms: u64,
    ) {
        let Some(record) = self
            .task_records
            .iter_mut()
            .find(|record| record.task_id == task_id)
        else {
            return;
        };
        record.status = SessionTaskStatus::Completed;
        record.summary = "执行完成".to_string();
        if let Some(plan_snapshot) = plan_snapshot {
            record.plan_snapshot = Some(plan_snapshot);
        }
        record.tool_result = tool_result;
        record.error = None;
        record.duration_ms = Some(duration_ms);
        let now = now_text();
        record.updated_at = now.clone();
        record.finished_at = Some(now);
        self.updated_at = now_text();
    }

    pub fn fail_task(
        &mut self,
        task_id: &str,
        summary: impl Into<String>,
        error: Option<String>,
        duration_ms: u64,
    ) {
        let Some(record) = self
            .task_records
            .iter_mut()
            .find(|record| record.task_id == task_id)
        else {
            return;
        };
        record.status = SessionTaskStatus::Failed;
        record.summary = summary.into();
        record.error = error;
        record.duration_ms = Some(duration_ms);
        let now = now_text();
        record.updated_at = now.clone();
        record.finished_at = Some(now);
        self.updated_at = now_text();
    }

    pub fn recover_interrupted_tasks(&mut self) -> usize {
        let mut recovered = 0usize;
        for record in &mut self.task_records {
            if matches!(
                record.status,
                SessionTaskStatus::Planning | SessionTaskStatus::Executing
            ) {
                recovered += 1;
                record.status = SessionTaskStatus::Failed;
                record.summary = "任务因进程中断而恢复为失败".to_string();
                record.error = Some("执行中断：应用重启或异常退出".to_string());
                let now = now_text();
                record.updated_at = now.clone();
                record.finished_at = Some(now);
            }
        }
        if recovered > 0 {
            self.updated_at = now_text();
        }
        recovered
    }

    pub fn recent_messages(&self, limit: usize) -> Vec<Message> {
        if self.messages.len() <= limit {
            return self.messages.clone();
        }
        self.messages[self.messages.len() - limit..].to_vec()
    }
}

pub fn now_text() -> String {
    Local::now().naive_local().to_string()
}

fn new_id() -> String {
    scru128::new().to_string()
}
