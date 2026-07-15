//! [`TiangongCore`] 的 Builder 模式构造入口。
//!
//! session 不再传入 Core——Core 从磁盘按需加载 session(每次 turn task)。

use std::sync::Arc;
use std::sync::mpsc::Sender;

use tiangong_types::SessionStreamEvent;

use crate::core::TiangongCore;
use crate::core::error::CoreError;
use crate::core::plugin::Plugin;
use crate::core_config::CoreConfigProvider;
use crate::permission::TrustMode;
use crate::session::Session;

/// [`TiangongCore`] 的构造器。
#[derive(Default)]
pub struct TiangongCoreBuilder {
    session_id: Option<String>,
    config: Option<CoreConfigProvider>,
    trust_mode: Option<TrustMode>,
    storage_root: Option<std::path::PathBuf>,
    event_sender: Option<Sender<SessionStreamEvent>>,
    plugins: Vec<Arc<dyn Plugin>>,
}

impl TiangongCoreBuilder {
    /// 会话 ID（必填）。
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// 模型配置提供者（必填）。
    pub fn config(mut self, config: CoreConfigProvider) -> Self {
        self.config = Some(config);
        self
    }

    /// 信任模式（必填）。
    pub fn trust_mode(mut self, trust_mode: TrustMode) -> Self {
        self.trust_mode = Some(trust_mode);
        self
    }

    /// 存储根目录（必填）。
    pub fn storage_root(mut self, storage_root: impl Into<std::path::PathBuf>) -> Self {
        self.storage_root = Some(storage_root.into());
        self
    }

    /// 流式事件发送端（必填）。会话级,跨 turn,clone 给每个 turn task。
    pub fn event_sender(mut self, sender: Sender<SessionStreamEvent>) -> Self {
        self.event_sender = Some(sender);
        self
    }

    /// 进程内插件（必填）。
    pub fn plugins(mut self, plugins: Vec<Arc<dyn Plugin>>) -> Self {
        self.plugins = plugins;
        self
    }

    /// 构造 core 实例。
    pub fn build(self) -> Result<TiangongCore, CoreError> {
        let session_id = self
            .session_id
            .ok_or(CoreError::MissingBuilderField("session_id"))?;
        let config = self
            .config
            .ok_or(CoreError::MissingBuilderField("config"))?;
        let trust_mode = self
            .trust_mode
            .ok_or(CoreError::MissingBuilderField("trust_mode"))?;
        let storage_root = self
            .storage_root
            .ok_or(CoreError::MissingBuilderField("storage_root"))?;
        let event_sender = self
            .event_sender
            .ok_or(CoreError::MissingBuilderField("event_sender"))?;
        Ok(TiangongCore::assemble(
            session_id,
            config,
            trust_mode,
            storage_root,
            event_sender,
            self.plugins,
        ))
    }
}

// Session 不再传入 Core,但保留这个方法供调用方先 persist session 再 build Core。
impl TiangongCoreBuilder {
    /// 从已有 session 提取 session_id + trust_mode 并 persist 到磁盘。
    ///
    /// 调用方在 build Core 前调用此方法,确保 session 文件已落盘。
    pub fn from_session(
        mut self,
        session: &Session,
        storage_root: impl Into<std::path::PathBuf>,
    ) -> Self {
        let storage_root = storage_root.into();
        self.session_id = Some(session.id.clone());
        self.trust_mode = Some(session.trust_mode);
        self.storage_root = Some(storage_root.clone());
        // persist session to disk
        let mut session = session.clone();
        session.bind_storage_root(storage_root);
        let _ = session.try_persist_to_disk();
        self
    }
}
