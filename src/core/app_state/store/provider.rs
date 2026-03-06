use super::super::*;

#[derive(Debug)]
pub(in crate::core::app_state) struct ProviderState {
    pub(in crate::core::app_state) model_config: ModelProviderConfig,
    pub(in crate::core::app_state) settings_api_auth_token_draft: String,
    pub(in crate::core::app_state) settings_api_base_url_draft: String,
    pub(in crate::core::app_state) settings_api_timeout_ms_draft: String,
    pub(in crate::core::app_state) settings_api_model_draft: String,
    pub(in crate::core::app_state) settings_model_list: Vec<String>,
}
