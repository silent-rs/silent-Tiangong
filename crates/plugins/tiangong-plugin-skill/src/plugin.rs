//! Skill 插件结构体定义与生命周期实现。
//!
//! [`SkillPlugin`] 自托管 [`SkillRegistry`]（扫描 `~/.tiangong/skills/`），
//! 不再依赖 `AgentConfig.skills`——skills 已从 AgentConfig 脱离，由本插件完全自治。
//!
//! `tool_specs()` / `prompt_sections()` / `handle_get_skill_detail` 均从自有的
//! `skill_registry` 读取，`register()` 阶段刷新缓存。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::paths::default_skills_storage_dir_path;
use crate::skill_registry::SkillRegistry;
use tiangong_core::core::Plugin;
use tiangong_core::core::plugin::PluginFeedbackTx;

/// Skill 插件。
///
/// 持有独立的 [`SkillRegistry`]，提供：
/// - `get_skill_detail` LLM 工具（经 ToolSpecProvider/ToolOverrideHandler）
/// - 已安装 Skills 的 system prompt 段落（经 PromptSectionProvider）
/// - App 管理 API（remove / set_enabled / refresh / list / detail）
pub struct SkillPlugin {
    /// 自托管的 Skill 注册表（扫描 `~/.tiangong/skills/`）。
    skill_registry: Arc<SkillRegistry>,
    /// 当前会话工作目录（由 core 注入）。
    workspace: RwLock<Option<PathBuf>>,
    /// 状态反馈通道（保持与其他插件一致的注入接口）。
    feedback_tx: RwLock<Option<PluginFeedbackTx>>,
}

impl SkillPlugin {
    /// 构造插件实例：自托管 SkillRegistry，扫描默认 skills 存储目录。
    pub fn new() -> Self {
        Self::with_storage_root(default_skills_storage_dir_path())
    }

    /// 用指定存储根目录构造（主要供测试或自定义路径使用）。
    pub fn with_storage_root(root: PathBuf) -> Self {
        let skill_registry = Arc::new(SkillRegistry::new(root));
        // 构造时刷新 registry 缓存，确保读到最新磁盘状态（原 register 逻辑迁入）。
        skill_registry.refresh();
        Self {
            skill_registry,
            workspace: RwLock::new(None),
            feedback_tx: RwLock::new(None),
        }
    }

    /// 取 SkillRegistry 的 Arc 引用（供 handler / management 使用）。
    pub(crate) fn registry(&self) -> Arc<SkillRegistry> {
        Arc::clone(&self.skill_registry)
    }
}

impl Default for SkillPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for SkillPlugin {
    fn id(&self) -> &str {
        "skill"
    }

    fn set_workspace(&self, workspace: Option<&std::path::Path>) {
        if let Ok(mut guard) = self.workspace.write() {
            *guard = workspace.map(|p| p.to_path_buf());
        }
    }

    fn set_feedback_tx(&self, tx: PluginFeedbackTx) {
        if let Ok(mut guard) = self.feedback_tx.write() {
            *guard = Some(tx);
        }
    }

    fn exec_env(&self) -> std::collections::BTreeMap<String, String> {
        // 贡献 enabled skill 目录的 .env.local 环境变量，供 run_command 子进程注入。
        let registry = self.registry();
        let view = registry.view();
        let mut env = std::collections::BTreeMap::new();
        for entry in view.entries.values() {
            let manifest_path = entry.dir.join("skill.toml");
            let Ok(manifest) = crate::skill_registry::read_skill_manifest(&manifest_path) else {
                continue;
            };
            if !manifest.available {
                continue;
            }
            for (key, value) in tiangong_core::runtime_env::load_local_env(&entry.dir) {
                env.insert(key, value);
            }
        }
        env
    }

    fn tool_permission_overrides(
        &self,
    ) -> std::collections::BTreeMap<String, tiangong_core::permission::PermissionLevel> {
        // get_skill_detail 是只读 skill 说明的工具，声明为 Safe，避免 core 硬编码。
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert(
            "get_skill_detail".to_string(),
            tiangong_core::permission::PermissionLevel::Safe,
        );
        overrides
    }
}
