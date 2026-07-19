use tiangong_core::model::SingleProviderClient;
use tiangong_llm::ModelEndpoint;
use tiangong_llm::models_config::RoutingSlot;

use super::super::*;

impl TiangongState {
    pub fn model_list(&self) -> &[String] {
        &self.store.provider.model_list
    }

    /// 当前 chat 模型名（从 registry 派生，issue #245）。
    pub fn current_model(&self) -> String {
        tiangong_config::registry::models()
            .resolve_slot(RoutingSlot::Chat)
            .map(|r| r.model.clone())
            .unwrap_or_default()
    }

    pub fn select_model(&mut self, model: &str) -> Result<()> {
        let api_model = model.trim();
        if api_model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空"));
        }
        // 经 registry 更新(含落盘 models.json),issue #245:不再 app-state 缓存。
        let mut models = tiangong_config::registry::models();
        models.update_chat_model(api_model.to_string());
        tiangong_config::registry::set_models(models);
        self.store.provider.model_list = normalize_model_list(
            self.store.provider.model_list.clone(),
            &self.current_model(),
        );
        self.replace_run_snapshot(
            RunStatus::Idle,
            format!("模型已切换：{}", self.current_model()),
            None,
        );
        self.persist_app_only()
    }

    pub fn refresh_model_list(&mut self) -> Result<usize> {
        let endpoint = tiangong_config::registry::models()
            .resolve_slot(RoutingSlot::Chat)
            .map(ModelEndpoint::from_resolved)
            .unwrap_or_default();
        let models = SingleProviderClient::list_models(&endpoint)?;
        self.store.provider.model_list = models;

        let current = self.current_model();
        let need_fill_default = current.trim().is_empty()
            || !self
                .store
                .provider
                .model_list
                .iter()
                .any(|m| m == current.trim());
        if need_fill_default && let Some(first) = self.store.provider.model_list.first().cloned() {
            let mut models = tiangong_config::registry::models();
            models.update_chat_model(first);
            tiangong_config::registry::set_models(models);
        }
        self.store.provider.model_list = normalize_model_list(
            self.store.provider.model_list.clone(),
            &self.current_model(),
        );
        self.persist_app_only()?;
        Ok(self.store.provider.model_list.len())
    }

    /// 更新版 ModelsConfig 并经 registry 落盘(issue #245:不再 app-state 缓存)。
    pub fn save_models_config(
        &mut self,
        new_config: tiangong_llm::models_config::ModelsConfig,
    ) -> Result<()> {
        tiangong_config::registry::set_models(new_config);
        self.replace_run_snapshot(
            RunStatus::Idle,
            format!("模型供应商已更新：{}", self.provider_label()),
            None,
        );
        self.persist_app_only()
    }
}
