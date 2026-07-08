//! AgentConfig 校验 facade。
//!
//! 扩展能力（外部工具、技能等）的管理 API 已由各自插件自托管；此处仅保留 core
//! 维护的 AgentConfig 校验入口。
use super::super::*;

impl TiangongState {
    /// 校验 agent 配置（扩展能力配置已脱离，此处仅保留接口供未来扩展）。
    pub fn validate_agent_config(&self) -> Result<()> {
        validate_agent_config(&self.store.agent.agent_config)
    }
}
