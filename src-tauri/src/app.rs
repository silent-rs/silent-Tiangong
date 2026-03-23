use std::sync::Mutex;

/// 天工应用状态
///
/// 持有核心的 TiangongState，通过 Mutex 实现线程安全
pub struct TiangongApp {
    pub state: Mutex<tiangong_core::app_state::TiangongState>,
}

impl TiangongApp {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(tiangong_core::app_state::TiangongState::load_or_default()),
        }
    }

    /// 获取状态的可变引用
    pub fn with_state<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut tiangong_core::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        self.state
            .lock()
            .map_err(|e| e.to_string())
            .and_then(|mut guard| f(&mut guard).map_err(|e| e.to_string()))
    }

    /// 获取状态的不可变引用
    pub fn with_state_read<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&tiangong_core::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        self.state
            .lock()
            .map_err(|e| e.to_string())
            .and_then(|guard| f(&guard).map_err(|e| e.to_string()))
    }
}
