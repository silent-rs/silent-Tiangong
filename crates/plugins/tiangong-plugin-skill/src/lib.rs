//! Skill 管理插件（get_skill_detail）。
//!
//! 将原 runtime 硬编码分发的 `get_skill_detail` 工具与 `inject_enhanced_tools` 中的
//! spec 注入收敛为独立插件 crate。同时承接原 `prompt::sections::build_skills_section`
//! 的 system prompt 段落（经 [`PromptSectionProvider`] 暴露）。
//!
//! 注册模式：入口层无条件注册本插件。插件内部在 `register` 阶段缓存 `AgentConfig`
//! 快照，`tool_specs()` 与 `prompt_sections()` 以「存在已启用 skill」为防御兜底——
//! 无启用 skill 时不暴露工具、不注入段落（与 `should_register` 等价但延迟到 register
//! 之后判断，避免入口层复制逻辑）。
//!
//! 注意：`install_skill` / `remove_skill` / `set_skill_enabled` 不在本插件范围内——
//! 它们是 app 层（Tauri 命令 / CLI modal / app_state facade）使用的 API，不作为 LLM
//! 工具暴露，其原 runtime spec 注入已作为死代码清理。

pub mod handler;
pub mod plugin;
pub mod prompt;

pub use plugin::SkillPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造 Skill 详情插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
///
/// 入口层无需任何条件判断直接调用——插件内部以 `register` 阶段缓存的 `AgentConfig`
/// 中是否存在已启用 skill 作为工具暴露与段落注入的兜底条件。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(SkillPlugin::new())
}

/// 构造默认的 Skill 详情插件列表，供各入口（CLI / Server / Tauri）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
