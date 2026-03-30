use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::model::TokenUsage;
use crate::planner::{PlanItem, PlanStepSource, PlanStepStatus};

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
    pub task_plans: Vec<SessionTaskPlan>,
    /// 会话级工作目录，工具执行时以此为根目录
    #[serde(default)]
    pub cwd: String,
    /// 早期对话的滚动摘要（用于无限上下文压缩）
    ///
    /// 当对话历史超过模型上下文阈值时，早期消息被 LLM 压缩为摘要存储在此。
    /// 构建 prompt 时注入为系统消息，原始 messages 保持完整供 UI 展示。
    /// 每次压缩会将旧摘要 + 新溢出消息折叠为新摘要，支持无限延伸。
    #[serde(default)]
    pub context_summary: Option<String>,
    /// 摘要覆盖到的消息索引（messages[0..summary_up_to] 已被摘要覆盖）
    #[serde(default)]
    pub summary_up_to: usize,
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
    /// 本次任务所有 LLM 调用的累计 token 用量
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPlanExecutionStep {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: PlanStepStatus,
    #[serde(default)]
    pub source: PlanStepSource,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTaskPlan {
    pub id: String,
    pub task_id: String,
    pub name: String,
    pub description: String,
    pub status: PlanStepStatus,
    #[serde(default)]
    pub execution_summary: Option<String>,
    #[serde(default)]
    pub execution_steps: Vec<SessionPlanExecutionStep>,
    pub created_at: String,
    pub updated_at: String,
}

impl Session {
    pub fn new(title: impl Into<String>) -> Self {
        let now = now_text();
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        Self {
            id: new_id(),
            title: title.into(),
            messages: Vec::new(),
            task_records: Vec::new(),
            task_plans: Vec::new(),
            cwd,
            context_summary: None,
            summary_up_to: 0,
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
            usage: None,
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

    pub fn bind_task_assistant_message_id(&mut self, task_id: &str, assistant_message_id: String) {
        let Some(record) = self
            .task_records
            .iter_mut()
            .find(|record| record.task_id == task_id)
        else {
            return;
        };
        record.assistant_message_id = assistant_message_id;
        record.updated_at = now_text();
        self.updated_at = now_text();
    }

    pub fn sync_task_plans(&mut self, task_id: &str, plans: &[PlanItem]) {
        for plan in plans {
            let now = now_text();
            if let Some(position) = self
                .task_plans
                .iter()
                .position(|item| item.id == plan.id && item.task_id == task_id)
            {
                let target = &mut self.task_plans[position];
                target.name = plan.name.clone();
                target.description = plan.description.clone();
                target.status = plan.status;
                target.execution_summary = plan.execution_summary.clone();
                target.updated_at = now.clone();

                let existing_steps = target.execution_steps.clone();
                let mut merged_steps = Vec::new();
                for step in &plan.execution_steps {
                    if let Some(found) = existing_steps.iter().find(|item| item.id == step.id) {
                        let mut step_record = found.clone();
                        step_record.name = step.name.clone();
                        step_record.description = step.description.clone();
                        step_record.status = step.status;
                        step_record.source = step.source;
                        step_record.updated_at = now.clone();
                        merged_steps.push(step_record);
                    } else {
                        merged_steps.push(SessionPlanExecutionStep {
                            id: step.id.clone(),
                            name: step.name.clone(),
                            description: step.description.clone(),
                            status: step.status,
                            source: step.source,
                            created_at: now.clone(),
                            updated_at: now.clone(),
                        });
                    }
                }
                target.execution_steps = merged_steps;
            } else {
                let mut target = SessionTaskPlan {
                    id: plan.id.clone(),
                    task_id: task_id.to_string(),
                    name: plan.name.clone(),
                    description: plan.description.clone(),
                    status: plan.status,
                    execution_summary: plan.execution_summary.clone(),
                    execution_steps: plan
                        .execution_steps
                        .iter()
                        .map(|step| SessionPlanExecutionStep {
                            id: step.id.clone(),
                            name: step.name.clone(),
                            description: step.description.clone(),
                            status: step.status,
                            source: step.source,
                            created_at: now.clone(),
                            updated_at: now.clone(),
                        })
                        .collect(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                };
                target.updated_at = now.clone();
                self.task_plans.push(target);
            }
        }
        self.updated_at = now_text();
    }

    pub fn delete_pending_task_plan(&mut self, pending_index: usize) -> bool {
        let Some(pos) = self
            .pending_task_plan_positions()
            .get(pending_index)
            .copied()
        else {
            return false;
        };
        self.task_plans.remove(pos);
        self.updated_at = now_text();
        true
    }

    pub fn move_pending_task_plan(&mut self, from_idx: usize, to_idx: usize) -> bool {
        let pending_positions = self.pending_task_plan_positions();
        if pending_positions.is_empty()
            || from_idx >= pending_positions.len()
            || to_idx >= pending_positions.len()
            || from_idx == to_idx
        {
            return false;
        }

        let mut pending = pending_positions
            .iter()
            .map(|idx| self.task_plans[*idx].clone())
            .collect::<Vec<_>>();
        let item = pending.remove(from_idx);
        pending.insert(to_idx, item);

        for (slot, item) in pending_positions.iter().zip(pending.into_iter()) {
            self.task_plans[*slot] = item;
        }
        self.updated_at = now_text();
        true
    }

    fn pending_task_plan_positions(&self) -> Vec<usize> {
        self.task_plans
            .iter()
            .enumerate()
            .filter_map(|(idx, plan)| (plan.status == PlanStepStatus::Pending).then_some(idx))
            .collect()
    }

    #[allow(dead_code)]
    pub fn complete_task(
        &mut self,
        task_id: &str,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
        duration_ms: u64,
    ) {
        self.complete_task_with_usage(task_id, plan_snapshot, tool_result, duration_ms, None);
    }

    pub fn complete_task_with_usage(
        &mut self,
        task_id: &str,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
        duration_ms: u64,
        usage: Option<TokenUsage>,
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
        record.usage = usage;
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
        self.fail_task_with_context(task_id, summary, error, duration_ms, None, None);
    }

    pub fn fail_task_with_context(
        &mut self,
        task_id: &str,
        summary: impl Into<String>,
        error: Option<String>,
        duration_ms: u64,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
    ) {
        self.fail_task_with_context_and_usage(
            task_id,
            summary,
            error,
            duration_ms,
            plan_snapshot,
            tool_result,
            None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fail_task_with_context_and_usage(
        &mut self,
        task_id: &str,
        summary: impl Into<String>,
        error: Option<String>,
        duration_ms: u64,
        plan_snapshot: Option<String>,
        tool_result: Option<String>,
        usage: Option<TokenUsage>,
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
        if let Some(plan_snapshot) = plan_snapshot {
            record.plan_snapshot = Some(plan_snapshot);
        }
        if tool_result.is_some() {
            record.tool_result = tool_result;
        }
        record.error = error;
        record.duration_ms = Some(duration_ms);
        record.usage = usage;
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

    /// 计算当前会话所有任务的累计 token 用量
    pub fn total_usage(&self) -> TokenUsage {
        let mut total = TokenUsage::default();
        for record in &self.task_records {
            if let Some(usage) = &record.usage {
                total.accumulate(usage);
            }
        }
        total
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
