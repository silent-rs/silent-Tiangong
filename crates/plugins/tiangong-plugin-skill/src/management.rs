//! App/CLI Skill 管理入口。
//!
//! 该模块作为 Skill 插件对外提供的管理 facade。当前阶段先把 App/Tauri/CLI 的调用入口
//! 收敛到插件 crate，保持外部命令名称与行为不变；底层状态变更仍复用 core 中已稳定的
//! TiangongState 能力，后续可继续把具体实现下沉到本插件内部。

use anyhow::Result;
use tiangong_core::app_state::{SkillInstallInspection, TiangongState};

/// Skill 管理扩展接口。
///
/// App/Tauri/CLI 侧应优先调用这里的方法，而不是直接调用 core app_state 的 Skill 管理方法。
pub trait SkillManagementExt {
    /// 检查 Skill 安装需求。
    fn skill_inspect_install_requirements(
        &self,
        path: &str,
        convert_external: bool,
    ) -> Result<SkillInstallInspection>;

    /// 安装本地 Skill。
    fn skill_install_local_with_options_and_inputs(
        &mut self,
        path: &str,
        enabled: bool,
        convert_external: bool,
        convert_env_values: &[(String, String)],
    ) -> Result<String>;

    /// 卸载 Skill。
    fn skill_remove(&mut self, id: &str) -> Result<String>;

    /// 启用或禁用 Skill。
    fn skill_set_enabled(&mut self, id: &str, enabled: bool) -> Result<String>;
}

impl SkillManagementExt for TiangongState {
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
}
