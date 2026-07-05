//! App/CLI Skill 管理入口。
//!
//! 该模块作为 Skill 插件对外提供的管理 facade。当前阶段先把 App/Tauri/CLI 的调用入口
//! 收敛到插件 crate，保持外部命令名称与行为不变；底层状态变更仍复用 core 中已稳定的
//! TiangongState 能力，后续可继续把具体实现下沉到本插件内部（真正自治）。
//!
//! App/Tauri/CLI 侧应优先调用这里的 `SkillManagementExt` 方法，而不是直接调用 core
//! app_state 的 Skill 管理方法——这样后续把实现搬进 plugin 时，调用方无需改动。

use std::sync::Arc;

use anyhow::Result;
use tiangong_core::agent_config::InstalledSkillConfig;
use tiangong_core::app_state::{SkillInstallInspection, TiangongState};
use tiangong_core::skill::{LoadedSkill, SkillRegistryView};

/// Skill 管理扩展接口。
///
/// 所有 App/Tauri/CLI 的 skill 管理操作应经此 trait，统一从 plugin crate 收敛入口。
/// 当前实现委托回 TiangongState，后续逐步把实现下沉到 plugin 内部。
pub trait SkillManagementExt {
    /// 已安装 Skill 列表（含启用与禁用）。
    fn skill_installed(&self) -> Vec<InstalledSkillConfig>;

    /// 注册表轻量视图（不含 SKILL.md 全文）。
    fn skill_list_view(&self) -> SkillRegistryView;

    /// Skill 完整详情（含 SKILL.md 全文），按需加载。
    fn skill_detail(&self, id: &str) -> Result<Arc<LoadedSkill>>;

    /// 初始化 skill 脚手架。
    fn skill_init_scaffold(
        &self,
        path: &str,
        name: Option<&str>,
        id: Option<&str>,
        force: bool,
    ) -> Result<String>;

    /// 检查 skill 安装需求。
    fn skill_inspect_install_requirements(
        &self,
        path: &str,
        convert_external: bool,
    ) -> Result<SkillInstallInspection>;

    /// 安装本地 skill。
    fn skill_install_local_with_options_and_inputs(
        &mut self,
        path: &str,
        enabled: bool,
        convert_external: bool,
        convert_env_values: &[(String, String)],
    ) -> Result<String>;

    /// 卸载 skill。
    fn skill_remove(&mut self, id: &str) -> Result<String>;

    /// 启用/禁用 skill。
    fn skill_set_enabled(&mut self, id: &str, enabled: bool) -> Result<String>;

    /// 手动重扫 skills 注册表。
    fn skill_refresh(&mut self) -> Result<String>;

    /// 检测/清理遗留的托管 MCP 配置与锁条目。
    fn skill_gc(&mut self, apply: bool) -> Result<String>;

    /// 诊断报告。
    fn skill_doctor(&mut self) -> Result<String>;
}

impl SkillManagementExt for TiangongState {
    fn skill_installed(&self) -> Vec<InstalledSkillConfig> {
        self.installed_skills()
    }

    fn skill_list_view(&self) -> SkillRegistryView {
        self.list_skills_view()
    }

    fn skill_detail(&self, id: &str) -> Result<Arc<LoadedSkill>> {
        self.get_skill_detail(id)
    }

    fn skill_init_scaffold(
        &self,
        path: &str,
        name: Option<&str>,
        id: Option<&str>,
        force: bool,
    ) -> Result<String> {
        self.init_skill_scaffold(path, name, id, force)
    }

    fn skill_inspect_install_requirements(
        &self,
        path: &str,
        convert_external: bool,
    ) -> Result<SkillInstallInspection> {
        self.inspect_skill_install_requirements(path, convert_external)
    }

    fn skill_install_local_with_options_and_inputs(
        &mut self,
        path: &str,
        enabled: bool,
        convert_external: bool,
        convert_env_values: &[(String, String)],
    ) -> Result<String> {
        self.install_local_skill_with_options_and_inputs(
            path,
            enabled,
            convert_external,
            convert_env_values,
        )
    }

    fn skill_remove(&mut self, id: &str) -> Result<String> {
        self.remove_skill(id)
    }

    fn skill_set_enabled(&mut self, id: &str, enabled: bool) -> Result<String> {
        self.set_skill_enabled(id, enabled)
    }

    fn skill_refresh(&mut self) -> Result<String> {
        self.refresh_skills()
    }

    fn skill_gc(&mut self, apply: bool) -> Result<String> {
        self.gc_skills(apply)
    }

    fn skill_doctor(&mut self) -> Result<String> {
        self.doctor_skills()
    }
}
