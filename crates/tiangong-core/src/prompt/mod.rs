//! Prompt 构建系统
//!
//! 通过 `build_full_system_prompt()` 构建完整的 system prompt 消息，
//! 包含身份、规则、自定义指令、环境信息、动态段和对话摘要。

pub mod sections;

pub use sections::{SubAgentPromptContext, SystemPromptConfig};
