use super::super::*;

impl TiangongState {
    pub fn installed_skills(&self) -> &[InstalledSkillConfig] {
        &self.store.agent.agent_config.skills.installed
    }

    pub fn init_skill_scaffold(
        &self,
        path: &str,
        name: Option<&str>,
        id: Option<&str>,
        force: bool,
    ) -> Result<String> {
        let service = self.services.skill_service;
        service.init_skill_scaffold(path, name, id, force)
    }

    pub fn install_local_skill(&mut self, path: &str, enabled: bool) -> Result<String> {
        self.install_local_skill_with_options(path, enabled, false)
    }

    pub fn inspect_skill_install_requirements(
        &self,
        path: &str,
        convert_external: bool,
    ) -> Result<SkillInstallInspection> {
        let service = self.services.skill_service;
        service.inspect_skill_install_requirements(self, path, convert_external)
    }

    pub fn install_local_skill_with_options(
        &mut self,
        path: &str,
        enabled: bool,
        convert_external: bool,
    ) -> Result<String> {
        self.install_local_skill_with_options_and_inputs(path, enabled, convert_external, &[])
    }

    pub fn install_local_skill_with_options_and_inputs(
        &mut self,
        path: &str,
        enabled: bool,
        convert_external: bool,
        convert_env_values: &[(String, String)],
    ) -> Result<String> {
        let service = self.services.skill_service;
        service.install_local_skill_with_options_and_inputs(
            self,
            path,
            enabled,
            convert_external,
            convert_env_values,
        )
    }

    pub fn remove_skill(&mut self, id: &str) -> Result<String> {
        let service = self.services.skill_service;
        service.remove_skill(self, id)
    }

    pub fn set_skill_enabled(&mut self, id: &str, enabled: bool) -> Result<String> {
        let service = self.services.skill_service;
        service.set_skill_enabled(self, id, enabled)
    }
}
