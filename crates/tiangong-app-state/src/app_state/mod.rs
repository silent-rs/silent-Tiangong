use std::collections::HashMap;

use tiangong_core::agent_config::AgentConfig;
use tiangong_core::runtime::RunSnapshot;

mod store;

pub use self::store::{InputCache, PendingTurnStub};
pub use tiangong_core_manager::CoreManager;

/// 应用状态数据。这里只定义数据，不执行应用操作或磁盘读写。
#[derive(Debug)]
pub struct TiangongState {
    pub config: tiangong_config::TiangongConfig,
    pub active_session_id: String,
    pub workspace_dir: String,
    pub input_caches: HashMap<String, InputCache>,
    pub model_list: Vec<String>,
    pub agent_config: AgentConfig,
    pub run: RunSnapshot,
    pub pending_turns: HashMap<String, PendingTurnStub>,
    pub core_manager: CoreManager,
}

impl TiangongState {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let config = tiangong_config::registry::init();
        let core_config = config.to_core_config();
        let core_manager = CoreManager::new(
            tiangong_core::core_config::CoreConfigProvider::new(core_config.clone()),
            config.storage_root.clone(),
        );
        let agent_config = AgentConfig {
            trust_mode: core_config.trust_mode,
            default_trust_mode: core_config.default_trust_mode,
            custom_system_prompt: core_config.custom_system_prompt,
            reasoning_effort: core_config.reasoning_effort,
        };
        Self {
            workspace_dir: config.workspace_dir.clone(),
            config,
            active_session_id: String::new(),
            input_caches: HashMap::new(),
            model_list: Vec::new(),
            agent_config,
            run: RunSnapshot::default(),
            pending_turns: HashMap::new(),
            core_manager,
        }
    }
}
