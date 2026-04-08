use std::collections::HashMap;
use std::sync::Mutex;

use tiangong_config::{CoreConfigProvider, load_tiangong_config};
use tiangong_core::core::TiangongCore;

/// 天工应用状态
///
/// state: 应用管理（会话列表、配置、持久化）
/// cores: 活跃的对话核心（session_id → TiangongCore）
/// config: 共享配置提供者（所有 Core 共享，GUI 修改配置后自动生效）
pub struct TiangongApp {
    pub state: Mutex<tiangong_core::app_state::TiangongState>,
    pub cores: Mutex<HashMap<String, TiangongCore>>,
    pub config: CoreConfigProvider,
}

impl Default for TiangongApp {
    fn default() -> Self {
        Self {
            state: Mutex::new(tiangong_core::app_state::TiangongState::load_or_default()),
            cores: Mutex::new(HashMap::new()),
            config: load_tiangong_config().into_core_config_provider(),
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
    ///
    /// 如果 core 已存在（多轮对话），直接复用。
    /// stream_tx 只在创建新 core 时使用。
    pub fn ensure_core(
        &self,
        session_id: &str,
        session: tiangong_core::session::Session,
        stream_tx: std::sync::mpsc::Sender<tiangong_types::StreamEvent>,
    ) -> (String, bool) {
        let mut cores = self.cores.lock().unwrap();
        if cores.contains_key(session_id) {
            return (session_id.to_string(), false); // 已存在，复用
        }
        let core = TiangongCore::with_session(self.config.clone(), session, stream_tx);
        let id = core.session_id().to_string();
        cores.insert(id.clone(), core);
        (id, true) // 新创建
    }

    /// 向指定会话的 core 发送消息
    pub fn send_to_core(&self, session_id: &str, content: String) {
        let cores = self.cores.lock().unwrap();
        if let Some(core) = cores.get(session_id) {
            core.send_message(content);
        }
    }

    /// 获取 core 的 session 快照（不消费 core）
    pub fn get_core_session(
        &self,
        session_id: &str,
    ) -> Option<tiangong_core::session::Session> {
        // core session 在消费线程中独占，无法直接读取
        // 只能在 into_session 时获取
        None
    }

    /// 取回 core 的 session（消费 core，用于持久化或切换会话）
    pub fn take_core(&self, session_id: &str) -> Option<TiangongCore> {
        let mut cores = self.cores.lock().unwrap();
        cores.remove(session_id)
    }

    /// 检查 session 是否有活跃 core
    pub fn is_session_executing(&self, session_id: &str) -> bool {
        let cores = self.cores.lock().unwrap();
        cores.contains_key(session_id)
    }
}
