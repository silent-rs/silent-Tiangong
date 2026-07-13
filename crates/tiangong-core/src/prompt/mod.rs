//! Prompt 构建系统
//!
//! core 只负责 Prompt 段落的顺序组装、会话运行上下文与摘要合并。
//! 产品文案由各插件经 `PromptSectionProvider` 注入（见 `tiangong-plugin-prompt`）。

pub mod sections;

pub use sections::SystemPromptConfig;
