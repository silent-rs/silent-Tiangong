use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::models_config::RoutingSlot;

use super::super::*;

impl TiangongState {
    pub fn model_list(&self) -> &[String] {
        &self.store.provider.model_list
    }

    pub fn current_model(&self) -> &str {
        &self.store.provider.model_endpoint.model
    }

    pub fn select_model(&mut self, model: &str) -> Result<()> {
        let api_model = model.trim();
        if api_model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空"));
        }

        // 更新 routing 中的 chat 模型
        self.store
            .provider
            .models_config
            .update_chat_model(api_model.to_string());
        let dir = tiangong_config::io::storage_root();
        let _ =
            tiangong_config::io::save_models_config_at(&dir, &self.store.provider.models_config);

        // 重新生成内部 model_endpoint
        self.store.provider.model_endpoint = self
            .store
            .provider
            .models_config
            .resolve_slot(RoutingSlot::Chat)
            .map(ModelEndpoint::from_resolved)
            .unwrap_or_default();
        self.store.provider.model_list = normalize_model_list(
            self.store.provider.model_list.clone(),
            &self.store.provider.model_endpoint.model,
        );
        self.rebuild_runtime_from_current_config();
        self.replace_run_snapshot(
            RunStatus::Idle,
            format!("模型已切换：{}", self.store.provider.model_endpoint.model),
            None,
        );
        self.persist_app_only()
    }

    pub fn refresh_model_list(&mut self) -> Result<usize> {
        let endpoint = self
            .store
            .provider
            .models_config
            .resolve_slot(RoutingSlot::Chat)
            .map(ModelEndpoint::from_resolved)
            .unwrap_or_default();
        let models = SingleProviderClient::list_models(&endpoint)?;
        self.store.provider.model_list = models;

        let current = self.store.provider.model_endpoint.model.trim().to_string();
        let need_fill_default =
            current.is_empty() || !self.store.provider.model_list.iter().any(|m| m == &current);
        if need_fill_default && let Some(first) = self.store.provider.model_list.first() {
            // 自动选择第一个模型
            self.store
                .provider
                .models_config
                .update_chat_model(first.clone());
            let dir = tiangong_config::io::storage_root();
            let _ = tiangong_config::io::save_models_config_at(
                &dir,
                &self.store.provider.models_config,
            );
            self.store.provider.model_endpoint = self
                .store
                .provider
                .models_config
                .resolve_slot(RoutingSlot::Chat)
                .map(ModelEndpoint::from_resolved)
                .unwrap_or_default();
        }
        self.store.provider.model_list = normalize_model_list(
            self.store.provider.model_list.clone(),
            self.store.provider.model_endpoint.model.trim(),
        );
        self.persist_app_only()?;

        Ok(self.store.provider.model_list.len())
    }

    /// 更新新版 ModelsConfig 并持久化到 models.json，同时同步内部状态
    pub fn save_models_config(
        &mut self,
        new_config: tiangong_core::models_config::ModelsConfig,
    ) -> Result<()> {
        let dir = tiangong_config::io::storage_root();
        tiangong_config::io::save_models_config_at(&dir, &new_config)?;
        self.store.provider.models_config = new_config;
        self.store.provider.model_endpoint = self
            .store
            .provider
            .models_config
            .resolve_slot(RoutingSlot::Chat)
            .map(ModelEndpoint::from_resolved)
            .unwrap_or_default();
        self.rebuild_runtime_from_current_config();
        self.replace_run_snapshot(
            RunStatus::Idle,
            format!(
                "模型供应商已更新：{}",
                self.services.runtime.provider_label()
            ),
            None,
        );
        self.persist_app_only()
    }
}
