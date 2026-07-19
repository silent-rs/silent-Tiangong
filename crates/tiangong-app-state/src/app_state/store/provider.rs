/// Provider 状态：仅保留 UI 持久化的可用模型列表。
///
/// issue #245:`models_config` 已归 config registry(进程单例),
/// `model_endpoint` 按需从 registry 派生,均不在 app-state 缓存。
#[derive(Debug, Default)]
pub struct ProviderState {
    /// UI 下拉列表用的可用模型名(持久化在 app.json)。
    pub model_list: Vec<String>,
}
