use super::super::*;

impl TiangongState {
    pub fn settings_api_base_url_draft(&self) -> &str {
        &self.store.provider.settings_api_base_url_draft
    }

    pub fn settings_api_timeout_ms_draft(&self) -> &str {
        &self.store.provider.settings_api_timeout_ms_draft
    }

    pub fn settings_api_model_draft(&self) -> &str {
        &self.store.provider.settings_api_model_draft
    }

    pub fn settings_model_list(&self) -> &[String] {
        &self.store.provider.settings_model_list
    }

    pub fn model_list(&self) -> &[String] {
        &self.store.provider.settings_model_list
    }

    pub fn current_model(&self) -> &str {
        &self.store.provider.model_config.api_model
    }

    pub fn select_model(&mut self, model: &str) -> Result<()> {
        let api_model = model.trim();
        if api_model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空"));
        }

        self.store.provider.model_config.api_model = api_model.to_string();
        self.store.provider.settings_api_model_draft = api_model.to_string();
        self.store.provider.settings_model_list = normalize_model_list(
            self.store.provider.settings_model_list.clone(),
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

    pub fn open_provider_settings(&mut self) {
        self.store.provider.settings_api_auth_token_draft =
            self.store.provider.model_config.api_auth_token.clone();
        self.store.provider.settings_api_base_url_draft =
            self.store.provider.model_config.api_base_url.clone();
        self.store.provider.settings_api_timeout_ms_draft =
            self.store.provider.model_config.api_timeout_ms.clone();
        self.store.provider.settings_api_model_draft =
            self.store.provider.model_config.api_model.clone();
    }

    pub fn update_settings_api_auth_token_draft(&mut self, value: String) {
        self.store.provider.settings_api_auth_token_draft = value;
    }

    pub fn update_settings_api_base_url_draft(&mut self, value: String) {
        self.store.provider.settings_api_base_url_draft = value;
    }

    pub fn update_settings_api_timeout_ms_draft(&mut self, value: String) {
        self.store.provider.settings_api_timeout_ms_draft = value;
    }

    pub fn update_settings_api_model_draft(&mut self, value: String) {
        self.store.provider.settings_api_model_draft = value;
    }

    pub fn refresh_model_list(&mut self) -> Result<usize> {
        let draft_config = ModelProviderConfig {
            api_auth_token: self
                .store
                .provider
                .settings_api_auth_token_draft
                .trim()
                .to_string(),
            api_base_url: self
                .store
                .provider
                .settings_api_base_url_draft
                .trim()
                .to_string(),
            api_timeout_ms: self
                .store
                .provider
                .settings_api_timeout_ms_draft
                .trim()
                .to_string(),
            api_model: self
                .store
                .provider
                .settings_api_model_draft
                .trim()
                .to_string(),
        };

        let models = SingleProviderClient::list_models(&draft_config)?;
        self.store.provider.settings_model_list = models;
        let draft_model = self.store.provider.settings_api_model_draft.trim();
        let need_fill_default = draft_model.is_empty()
            || !self
                .store
                .provider
                .settings_model_list
                .iter()
                .any(|m| m == draft_model);
        if need_fill_default && let Some(first) = self.store.provider.settings_model_list.first() {
            self.store.provider.settings_api_model_draft = first.clone();
        }
        self.store.provider.settings_model_list = normalize_model_list(
            self.store.provider.settings_model_list.clone(),
            self.store.provider.settings_api_model_draft.trim(),
        );
        self.persist_app_only()?;

        Ok(self.store.provider.settings_model_list.len())
    }

    pub fn discard_provider_settings(&mut self) {
        self.store.provider.settings_api_auth_token_draft =
            self.store.provider.model_config.api_auth_token.clone();
        self.store.provider.settings_api_base_url_draft =
            self.store.provider.model_config.api_base_url.clone();
        self.store.provider.settings_api_timeout_ms_draft =
            self.store.provider.model_config.api_timeout_ms.clone();
        self.store.provider.settings_api_model_draft =
            self.store.provider.model_config.api_model.clone();
    }

    pub fn save_provider_settings(&mut self) -> Result<()> {
        let api_auth_token = self.store.provider.settings_api_auth_token_draft.trim();
        let api_base_url = self.store.provider.settings_api_base_url_draft.trim();
        let api_timeout_ms = self.store.provider.settings_api_timeout_ms_draft.trim();
        let api_model = self.store.provider.settings_api_model_draft.trim();

        if api_auth_token.is_empty() {
            return Err(anyhow!("API_AUTH_TOKEN 不能为空"));
        }
        if api_base_url.is_empty() {
            return Err(anyhow!("API_BASE_URL 不能为空"));
        }
        if api_timeout_ms.is_empty() {
            return Err(anyhow!("API_TIMEOUT_MS 不能为空"));
        }
        if api_timeout_ms.parse::<u64>().is_err() {
            return Err(anyhow!("API_TIMEOUT_MS 必须是毫秒数值"));
        }
        if api_model.is_empty() {
            return Err(anyhow!("API_MODEL 不能为空"));
        }

        self.store.provider.model_config = ModelProviderConfig {
            api_auth_token: api_auth_token.to_string(),
            api_base_url: api_base_url.to_string(),
            api_timeout_ms: api_timeout_ms.to_string(),
            api_model: api_model.to_string(),
        };
        self.store.provider.settings_model_list = normalize_model_list(
            self.store.provider.settings_model_list.clone(),
            &self.store.provider.model_config.api_model,
        );
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
