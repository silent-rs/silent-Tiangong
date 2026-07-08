use super::super::*;

impl TiangongState {
    /// 发送当前输入（已迁移到 TiangongCore.send_message）
    pub fn send_current_input(&mut self) -> Result<()> {
        let service = self.services.turn_service;
        service.send_current_input(self)
    }
}
