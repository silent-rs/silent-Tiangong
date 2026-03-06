mod agent;
mod provider;
mod runtime;
mod session;

pub(in crate::core::app_state) use agent::AgentState;
pub(in crate::core::app_state) use provider::ProviderState;
pub(in crate::core::app_state) use runtime::RuntimeState;
pub(in crate::core::app_state) use session::SessionState;

#[derive(Debug)]
pub(in crate::core::app_state) struct AppStore {
    pub(in crate::core::app_state) session: SessionState,
    pub(in crate::core::app_state) provider: ProviderState,
    pub(in crate::core::app_state) agent: AgentState,
    pub(in crate::core::app_state) runtime: RuntimeState,
}
