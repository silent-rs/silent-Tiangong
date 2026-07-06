//! Agent 辅助模块。
//!
//! 历史：本目录曾承载 planning/execution/response 三段式旧流水线的各个
//! "Agent"。随着 `ReactEngine` 成为唯一执行入口，旧流水线已退场，仅保留
//! 仍被活跃主链路复用的提示词、工具/MCP 转换与技能转换辅助。
//!
//! MCP 工具执行适配（`execution_mcp_agent`）已整块迁出至
//! `tiangong-plugin-mcp` crate，core 不再持有 MCP 概念。
pub mod skill_convert_agent;
