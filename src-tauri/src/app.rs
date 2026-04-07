use std::collections::HashMap;
use std::sync::Mutex;

use tiangong_core::core::TiangongCore;

/// 天工应用状态
///
/// state: 应用管理（会话列表、配置、持久化）
/// cores: 活跃的对话核心（session_id → TiangongCore）
pub struct TiangongApp {
    pub state: Mutex<tiangong_core::app_state::TiangongState>,
    pub cores: Mutex<HashMap<String, TiangongCore>>,
}

impl Default for TiangongApp {
    fn default() -> Self {
        Self {
            state: Mutex::new(tiangong_core::app_state::TiangongState::load_or_default()),
            cores: Mutex::new(HashMap::new()),
        }
    }
}

impl TiangongApp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_state<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut tiangong_core::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        self.state
            .lock()
            .map_err(|e| e.to_string())
            .and_then(|mut guard| f(&mut guard).map_err(|e| e.to_string()))
    }

    pub fn with_state_read<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&tiangong_core::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        self.state
            .lock()
            .map_err(|e| e.to_string())
            .and_then(|guard| f(&guard).map_err(|e| e.to_string()))
    }

    /// 获取或创建会话对应的 TiangongCore
    pub fn get_or_create_core(
        &self,
        session_id: &str,
        session: tiangong_core::session::Session,
        stream_tx: std::sync::mpsc::Sender<tiangong_types::StreamEvent>,
    ) -> String {
        let mut cores = self.cores.lock().unwrap();
        if !cores.contains_key(session_id) {
            let core = TiangongCore::with_session(session, stream_tx);
            let id = core.session_id().to_string();
            cores.insert(id.clone(), core);
            return id;
        }
        session_id.to_string()
    }

    /// 向指定会话的 core 发送消息
    pub fn send_to_core(&self, session_id: &str, content: String) {
        let cores = self.cores.lock().unwrap();
        if let Some(core) = cores.get(session_id) {
            core.send_message(content);
        }
    }

    /// 取回 core 的 session（消费 core）
    pub fn take_core(&self, session_id: &str) -> Option<TiangongCore> {
        let mut cores = self.cores.lock().unwrap();
        cores.remove(session_id)
    }
}
