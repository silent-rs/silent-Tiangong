use serde::{Deserialize, Serialize};

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
