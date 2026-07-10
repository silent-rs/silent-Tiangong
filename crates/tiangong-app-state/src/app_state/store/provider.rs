use tiangong_llm::ModelEndpoint;
use tiangong_llm::models_config::ModelsConfig;

#[derive(Debug)]
pub struct ProviderState {
    pub models_config: ModelsConfig,
    pub model_endpoint: ModelEndpoint, // 从 models_config 自动生成（内部用）
    pub model_list: Vec<String>,
}
