use serde::{Deserialize, Serialize};

use crate::model::ToolSpec;

/// plan 步骤完成控制工具名。
pub const MARK_STEP_COMPLETED_TOOL: &str = "mark_step_completed";

/// plan 步骤完成控制的工具规格。
///
/// 供 Agent 标记当前执行步骤完成、并声明是否继续追加动态步骤。归属 plan 执行控制，
/// 而非本地工具能力。
///
/// 当前未注入 LLM tools 列表（plan 执行控制走其他路径），保留定义供将来按需启用。
#[allow(dead_code)]
pub(crate) fn mark_step_completed_tool_spec() -> ToolSpec {
    ToolSpec {
        name: MARK_STEP_COMPLETED_TOOL.to_string(),
        description: "标记当前执行步骤已完成。仅在本步骤真正完成后调用。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "result": { "type": "string", "description": "本步骤完成结果摘要" },
                "continue_execution": {
                    "type": "boolean",
                    "description": "当前 plan 是否需要继续执行后续动态步骤。true 表示需要追加下一步。"
                },
                "next_step_name": {
                    "type": "string",
                    "description": "当 continue_execution=true 时，下一步名称（必填）。"
                },
                "next_step_description": {
                    "type": "string",
                    "description": "当 continue_execution=true 时，下一步描述（必填）。"
                }
            },
            "required": ["continue_execution"]
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub status: PlanStepStatus,
    #[serde(default)]
    pub source: PlanStepSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub status: PlanStepStatus,
    #[serde(default)]
    pub execution_summary: Option<String>,
    #[serde(default)]
    pub execution_steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlanStepStatus {
    #[default]
    Pending,
    Completed,
    Failed,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlanStepSource {
    #[default]
    Planned,
    Dynamic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRevision {
    pub id: String,
    pub phase: String,
    pub reason: String,
    pub summary_after_revision: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskPlan {
    #[serde(default)]
    pub id: String,
    pub objective: String,
    pub summary: String,
    pub plans: Vec<PlanItem>,
    pub risks: Vec<String>,
    pub skill_hints: Vec<String>,
    pub mcp_hints: Vec<String>,
    #[serde(default)]
    pub revisions: Vec<PlanRevision>,
}

impl PlanItem {
    pub fn refresh_status(&mut self) {
        let has_failed = self
            .execution_steps
            .iter()
            .any(|step| step.status == PlanStepStatus::Failed);
        let has_pending = self
            .execution_steps
            .iter()
            .any(|step| step.status == PlanStepStatus::Pending);
        let all_ignored = !self.execution_steps.is_empty()
            && self
                .execution_steps
                .iter()
                .all(|step| step.status == PlanStepStatus::Ignored);
        let all_finished = !self.execution_steps.is_empty()
            && self
                .execution_steps
                .iter()
                .all(|step| step.status != PlanStepStatus::Pending);

        self.status = if has_failed {
            PlanStepStatus::Failed
        } else if has_pending {
            PlanStepStatus::Pending
        } else if all_ignored {
            PlanStepStatus::Ignored
        } else if all_finished {
            PlanStepStatus::Completed
        } else {
            PlanStepStatus::Pending
        };
    }
}

impl TaskPlan {
    pub fn revise(
        &mut self,
        phase: impl Into<String>,
        reason: impl Into<String>,
        summary_after_revision: impl Into<String>,
    ) {
        let summary_after_revision = summary_after_revision.into();
        self.revisions.push(PlanRevision {
            id: new_id(),
            phase: phase.into(),
            reason: reason.into(),
            summary_after_revision: summary_after_revision.clone(),
        });
        self.summary = summary_after_revision;
    }

    pub fn ensure_risk(&mut self, risk: impl Into<String>) {
        let risk = risk.into();
        if !self.risks.iter().any(|existing| existing == &risk) {
            self.risks.push(risk);
        }
    }

    pub fn push_plan(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        execution_steps: Vec<PlanStep>,
    ) {
        let mut item = PlanItem {
            id: new_id(),
            name: name.into(),
            description: description.into(),
            status: PlanStepStatus::Pending,
            execution_summary: None,
            execution_steps,
        };
        item.refresh_status();
        self.plans.push(item);
    }

    pub fn refresh_plan_statuses(&mut self) {
        for plan in &mut self.plans {
            plan.refresh_status();
        }
    }
}

fn new_id() -> String {
    scru128::new().to_string()
}
