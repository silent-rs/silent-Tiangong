/// @提及候选（见 [`tiangong_types::MentionCandidate`]）。
pub use tiangong_types::MentionCandidate;

pub mod agent_config;
pub mod agent_input;
pub mod context;
pub mod core;
pub mod core_config;
pub(crate) mod formatting;
pub mod media;
pub mod model;
pub mod models_config;
pub mod observe;
pub mod permission;
pub mod planner;
pub mod prompt;
pub mod react;
pub mod runtime;
pub mod runtime_env;
pub mod session;
pub mod shared_runtime;
mod stream_throttle;
pub mod tool;
pub mod tool_override;
pub mod turn_context;
