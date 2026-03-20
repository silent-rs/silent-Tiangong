use super::super::*;

#[derive(Debug)]
pub struct ProviderState {
    pub model_config: ModelProviderConfig,
    pub settings_api_auth_token_draft: String,
    pub settings_api_base_url_draft: String,
    pub settings_api_timeout_ms_draft: String,
    pub settings_api_model_draft: String,
    pub settings_model_list: Vec<String>,
}
