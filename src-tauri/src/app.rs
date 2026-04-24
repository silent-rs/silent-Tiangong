use std::collections::HashMap;
use std::sync::Mutex;

use tiangong_config::load_tiangong_config;
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::CoreConfigProvider;

/// 天工应用状态
///
/// state: 应用管理（会话列表、配置、持久化）
/// cores: 活跃的对话核心（session_id → TiangongCore）
/// config: 共享配置提供者
pub struct TiangongApp {
    pub state: Mutex<tiangong_core::app_state::TiangongState>,
    pub cores: Mutex<HashMap<String, TiangongCore>>,
    pub config: CoreConfigProvider,
}

impl Default for TiangongApp {
    fn default() -> Self {
        let app_config = load_tiangong_config();
        let core_config = app_config.to_core_config();
        let config = CoreConfigProvider::new(core_config);

        Self {
            state: Mutex::new(tiangong_core::app_state::TiangongState::load_or_default()),
            cores: Mutex::new(HashMap::new()),
            config,
        }
    }
}

impl TiangongApp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sync_core_config_from_state(&self) -> Result<(), String> {
        let base = self.config.snapshot();
        let next =
            self.with_state_read(|core_state| Ok(core_state.build_core_config_from_base(&base)))?;
        self.config.replace(next);
        Ok(())
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
        stream_tx: std::sync::mpsc::Sender<tiangong_types::SessionStreamEvent>,
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
    pub fn send_to_core(&self, session_id: &str, content: String) -> bool {
        self.send_to_core_with_id(session_id, content, None)
    }

    /// 向指定会话的 core 发送带固定消息 ID 的消息
    pub fn send_to_core_with_id(
        &self,
        session_id: &str,
        content: String,
        message_id: Option<String>,
    ) -> bool {
        let cores = self.cores.lock().unwrap();
        if let Some(core) = cores.get(session_id) {
            if let Some(message_id) = message_id {
                core.send_message_with_id(content, message_id);
            } else {
                core.send_message(content);
            }
            true
        } else {
            false
        }
    }

    /// 获取 core 的 session 快照（不消费 core）
    pub fn get_core_session(&self, _session_id: &str) -> Option<tiangong_core::session::Session> {
        // core session 在消费线程中独占，无法直接读取
        // 只能在 into_session 时获取
        None
    }

    /// 取回 core 的 session（消费 core，用于持久化或切换会话）
    pub fn take_core(&self, session_id: &str) -> Option<TiangongCore> {
        let mut cores = self.cores.lock().unwrap();
        cores.remove(session_id)
    }

    /// 取消指定会话的执行
    pub fn cancel_core(&self, session_id: &str) {
        let cores = self.cores.lock().unwrap();
        if let Some(core) = cores.get(session_id) {
            core.cancel();
        }
    }

    /// 向指定会话的 core 发送审批响应
    pub fn respond_approval_to_core(&self, session_id: &str, request_id: String, approved: bool) {
        let cores = self.cores.lock().unwrap();
        if let Some(core) = cores.get(session_id) {
            core.respond_approval(request_id, approved);
        }
    }

    /// 设置所有活跃 core 的信任模式（全局生效）
    pub fn set_all_cores_trust_mode(&self, mode: tiangong_core::permission::TrustMode) {
        let cores = self.cores.lock().unwrap();
        for core in cores.values() {
            core.set_trust_mode(mode);
        }
    }

    /// 设置指定会话 core 的信任模式（实时生效）
    pub fn set_core_trust_mode(
        &self,
        session_id: &str,
        mode: tiangong_core::permission::TrustMode,
    ) {
        let cores = self.cores.lock().unwrap();
        if let Some(core) = cores.get(session_id) {
            core.set_trust_mode(mode);
        }
    }

    /// 检查 session 是否有活跃 core
    pub fn is_session_executing(&self, session_id: &str) -> bool {
        let cores = self.cores.lock().unwrap();
        cores.contains_key(session_id)
    }
}
