//! Turn 服务启动逻辑（已迁移到 TiangongCore）
//!
//! TiangongCore 作为统一核心，直接处理消息发送、工具执行、事件输出。
//! 此模块保留空壳以维持 AppTurnService 结构。

use super::*;

impl AppTurnService {
    /// 发送当前输入（已迁移到 TiangongCore.send_message）
    pub(in crate::app_state) fn send_current_input(self, state: &mut TiangongState) -> Result<()> {
        let input = state.store.session.input_draft.trim().to_string();
        if input.is_empty() {
            return Ok(());
        }
        // TiangongCore 直接处理消息发送
        tracing::debug!("send_current_input 已迁移到 TiangongCore");
        state.store.session.input_draft.clear();
        Ok(())
    }
}
