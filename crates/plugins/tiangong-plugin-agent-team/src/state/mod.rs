mod descriptor;
mod file_lock;

pub use descriptor::{AgentDescriptor, AgentStatus};
pub use file_lock::{FileLock, FileLockManager};
