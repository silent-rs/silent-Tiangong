mod client;
mod config;
mod context;
mod util;

pub use config::{describe_mcp_servers, summarize_mcp_servers, validate_mcp_config};
pub use context::{build_mcp_hints, collect_mcp_context};

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
