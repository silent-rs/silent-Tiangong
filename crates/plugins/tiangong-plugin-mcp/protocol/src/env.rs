//! exec_env 回传操作（收集启用 MCP server 的环境变量供 run_command 注入）。

use crate::tool::EnvMap;
use crate::{Empty, McpOperation};

pub const ENV_COLLECT_OPERATION: &str = "mcp.env.collect";

pub struct CollectEnv;
impl McpOperation for CollectEnv {
    const NAME: &'static str = ENV_COLLECT_OPERATION;
    type Request = Empty;
    type Response = EnvMap;
}

#[allow(dead_code)]
fn _ensure_empty_used(_e: Empty) {}
