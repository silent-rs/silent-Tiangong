use super::super::*;

impl TiangongState {
    pub fn model_list(&self) -> &[String] {
        &self.store.provider.model_list
    }

    pub fn current_model(&self) -> &str {
        &self.store.provider.model_config.api_model
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
        let _ = self.store.provider.models_config.save();

        // 重新生成内部 model_config
        self.store.provider.model_config =
            self.store.provider.models_config.to_chat_provider_config();
        self.store.provider.model_list = normalize_model_list(
            self.store.provider.model_list.clone(),
            &self.store.provider.model_config.api_model,
        );
        self.rebuild_runtime_from_current_config();
        self.replace_run_snapshot(
            RunStatus::Idle,
            format!("模型已切换：{}", self.store.provider.model_config.api_model),
            None,
        );
        self.persist_app_only()
    }

    pub fn refresh_model_list(&mut self) -> Result<usize> {
        let config = self.store.provider.models_config.to_chat_provider_config();
        let models = SingleProviderClient::list_models(&config)?;
        self.store.provider.model_list = models;

        let current = self
            .store
            .provider
            .model_config
            .api_model
            .trim()
            .to_string();
        let need_fill_default =
            current.is_empty() || !self.store.provider.model_list.iter().any(|m| m == &current);
        if need_fill_default && let Some(first) = self.store.provider.model_list.first() {
            // 自动选择第一个模型
            self.store
                .provider
                .models_config
                .update_chat_model(first.clone());
            let _ = self.store.provider.models_config.save();
            self.store.provider.model_config =
                self.store.provider.models_config.to_chat_provider_config();
        }
        self.store.provider.model_list = normalize_model_list(
            self.store.provider.model_list.clone(),
            self.store.provider.model_config.api_model.trim(),
        );
        self.persist_app_only()?;

        Ok(self.store.provider.model_list.len())
    }

    /// 更新新版 ModelsConfig 并持久化到 models.json，同时同步内部状态
    pub fn save_models_config(
        &mut self,
        new_config: tiangong_core::models_config::ModelsConfig,
    ) -> Result<()> {
        new_config.save()?;
        self.store.provider.models_config = new_config;
        self.store.provider.model_config =
            self.store.provider.models_config.to_chat_provider_config();
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
