//! Skill 管理插件。
//!
//! 承载 Skill 相关的全部能力：
//! - **LLM 工具**：`get_skill_detail`（查看 skill 说明）
//! - **System Prompt 段落**：已安装 Skills 摘要 + Skill 创建规范（引导 Agent 用文件工具
//!   在 skills 目录下编写 `skill.toml` + `SKILL.md`）
//! - **App 管理 API**：[`SkillPlugin`] 直接提供 remove / set_enabled / refresh /
//!   list / detail 方法，供 App/Tauri/CLI 调用（入口层持有插件实例）
//!
//! skills 已从 `tiangong_core::agent_config::AgentConfig` 彻底脱离，由本插件自托管
//! [`crate::skill_registry::SkillRegistry`]。`collect_runtime_env` 直接扫描
//! `~/.tiangong/skills/` 读 skill env，不经 agent_config 流转。
//!
//! Skill 创建/安装不提供专用工具；由 prompt 引导 Agent 使用文件工具在 skills 目录下
//! 创建 `skill.toml` 和 `SKILL.md`。

pub mod handler;
pub mod management;
pub mod mcp_lock;
pub mod paths;
pub mod plugin;
pub mod prompt;
pub mod skill_analysis;
pub mod skill_config;
pub mod skill_context;
pub mod skill_init;
pub mod skill_package;
pub mod skill_registry;
pub mod skill_util;

// Skill 领域类型 re-export（Skill 概念已从 core 完整迁入本 plugin）。
pub use skill_config::{
    InstalledSkillConfig, SkillMcpRequirementConfig, SkillPermissionConfig, SkillSourceConfig,
    SkillsConfig,
};
pub use skill_init::init_tiangong_skill_scaffold;
pub use skill_package::{load_skill_from_local_dir, prepare_skill_source_for_install};
pub use skill_registry::{
    LoadedSkill, SkillManifest, SkillRegistry, SkillRegistryEntry, SkillRegistryView,
    read_skill_manifest, scan_skill_registry,
};

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
