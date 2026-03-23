use super::super::super::*;

impl TiangongState {
    pub fn sessions(&self) -> &[Session] {
        &self.store.session.sessions
    }

    pub fn active_session_id(&self) -> &str {
        &self.store.session.active_session_id
    }

    pub fn active_session(&self) -> Option<&Session> {
        self.store
            .session
            .sessions
            .iter()
            .find(|session| session.id == self.store.session.active_session_id)
    }

    pub fn active_task_plans(&self) -> Vec<SessionTaskPlan> {
        let Some(session) = self.active_session() else {
            return Vec::new();
        };
        session.task_plans.to_vec()
    }

    pub fn has_pending_turn(&self) -> bool {
        self.store.runtime.pending_turn.is_some()
    }

    pub fn update_draft(&mut self, value: String) {
        self.store.session.input_draft = value;
    }

    pub fn provider_label(&self) -> String {
        self.services.runtime.provider_label()
    }

    pub fn models_config(&self) -> &crate::models_config::ModelsConfig {
        &self.store.provider.models_config
    }

    pub fn session_title_draft(&self) -> &str {
        &self.store.session.session_title_draft
    }

    pub fn update_session_title_draft(&mut self, value: String) {
        self.store.session.session_title_draft = value;
    }
}
