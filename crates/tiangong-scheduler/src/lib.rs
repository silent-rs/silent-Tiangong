pub mod executor;
pub mod model;
pub mod store;
pub mod webhook;

pub use executor::SchedulerContext;

pub(crate) const fn default_true() -> bool {
    true
}
