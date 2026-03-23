mod agent;
mod provider;
mod runtime;
mod session;

pub use agent::AgentState;
pub use provider::ProviderState;
pub use runtime::RuntimeState;
pub use session::SessionState;

#[derive(Debug)]
pub struct AppStore {
    pub session: SessionState,
    pub provider: ProviderState,
    pub agent: AgentState,
    pub runtime: RuntimeState,
}
