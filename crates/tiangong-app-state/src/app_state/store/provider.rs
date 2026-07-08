use super::super::*;
use tiangong_core::models_config::ModelsConfig;

#[derive(Debug)]
pub struct ProviderState {
    pub models_config: ModelsConfig,
    pub model_config: ModelProviderConfig, // 从 models_config 自动生成（内部用）
    pub model_list: Vec<String>,
}
