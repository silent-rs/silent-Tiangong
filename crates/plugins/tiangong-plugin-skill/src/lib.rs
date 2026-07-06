//! Skill 管理插件。
//!
//! 承载 Skill 相关的全部能力：
//! - **LLM 工具**：`get_skill_detail`（查看 skill 说明）、`install_skill`（agent 编写并安装 skill）
//! - **System Prompt 段落**：已安装 Skills 摘要（经 [`PromptSectionProvider`]）
//! - **App 管理 API**：[`SkillPlugin`] 直接提供 remove / set_enabled / refresh / gc /
//!   doctor / list / detail 方法，供 App/Tauri/CLI 调用（入口层持有插件实例）
//!
//! skills 已从 [`tiangong_core::agent_config::AgentConfig`] 彻底脱离，由本插件自托管
//! [`tiangong_core::skill::SkillRegistry`]。`collect_runtime_env` 直接扫描
//! `~/.tiangong/skills/` 读 skill env，不经 agent_config 流转。
//!
//! 安装统一经 agent 的 `install_skill` 工具（内容式：agent 编写 SKILL.md 正文 → 落地），
//! 不再支持固定路径安装。

pub mod handler;
pub mod management;
pub mod plugin;
pub mod prompt;

pub use plugin::SkillPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造 Skill 插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
///
/// 入口层无需条件判断直接调用——插件自托管 SkillRegistry，`tool_specs()` /
/// `prompt_sections()` 以 registry 中是否存在 available skill 作为兜底。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(SkillPlugin::new())
}

/// 构造默认的 Skill 详情插件列表，供各入口（CLI / Server / Tauri）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
