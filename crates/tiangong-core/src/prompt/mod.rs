//! Prompt 分层装配系统
//!
//! 参考：`docs/archive/prompt-organization-analysis.md`
//!
//! 将 prompt 视为"可编排的上下文结构"，而不是"固定模板 + 聊天历史"。
//!
//! 分层结构：
//! 1. 静态 System Prompt：身份 + 规则（缓存友好，极少变化）
//! 2. 动态 System Sections：环境信息、MCP 指令等（会话级缓存）
//! 3. System Context：工作目录、允许路径等环境事实
//! 4. User Context：用户偏好、记忆（system-reminder 方式注入）
//! 5. 历史消息：经过裁剪/压缩的对话历史
//! 6. Attachment Messages：技能列表、文件引用、诊断等高波动内容
//! 7. Tool Schemas：工具定义（session 级缓存）
//! 8. 当前用户输入

pub mod assembler;
pub mod sections;
pub mod types;

pub use assembler::PromptAssembler;
pub use types::{AssembledPrompt, PromptSection, SystemPromptBlock};
