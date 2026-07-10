//! [`TiangongCore`] 的 Builder 模式构造入口。
//!
//! 收敛此前 7 个入口命名构造方法（`new` / `new_for_cli` / `new_for_process` /
//! `with_session` / `with_session_for_gui` / `with_session_for_server` /
//! `with_session_for_process`），它们最终都委托到同一实现，core 内部没有任何
//! 入口差异语义。Builder 让构造过程与宿主入口（GUI/CLI/Server）完全解耦——
//! core 不再感知"是谁在构造它"。
//!
//! `session` 为必填字段：新会话由外部创建后传入，core 不再持有产品文案
//! （此前的 `Session::new("新对话")` 硬编码已移除）。

use std::sync::Arc;
use std::sync::mpsc::Sender;

use tiangong_types::SessionStreamEvent;

use crate::core::TiangongCore;
use crate::core::error::CoreError;
use crate::core::plugin::Plugin;
use crate::core::storage_location::CoreStorageLocation;
use crate::core_config::CoreConfigProvider;
use crate::session::Session;

/// [`TiangongCore`] 的构造器。
///
/// 通过 [`TiangongCore::builder()`] 获取实例，链式设置必填字段后调用
/// [`build`](Self::build) 得到一个可接收输入的 core 实例。
#[derive(Default)]
pub struct TiangongCoreBuilder {
    config: Option<CoreConfigProvider>,
    session: Option<Session>,
    stream_tx: Option<Sender<SessionStreamEvent>>,
    plugins: Vec<Arc<dyn Plugin>>,
    storage: Option<CoreStorageLocation>,
}

impl TiangongCoreBuilder {
    /// 模型配置提供者（必填）。
    pub fn config(mut self, config: CoreConfigProvider) -> Self {
        self.config = Some(config);
        self
    }

    /// 会话（必填）。新会话由调用方创建后传入，core 不自行构造。
    pub fn session(mut self, session: Session) -> Self {
        self.session = Some(session);
        self
    }

    /// 流式事件发送端（必填）。
    pub fn event_sender(mut self, stream_tx: Sender<SessionStreamEvent>) -> Self {
        self.stream_tx = Some(stream_tx);
        self
    }

    /// 进程内自注册插件（如定时任务插件），传 `Vec::new()` 表示不启用。
    pub fn plugins(mut self, plugins: Vec<Arc<dyn Plugin>>) -> Self {
        self.plugins = plugins;
        self
    }

    /// 存储位置（必填）。外部决定路径，core 负责路径下的存储过程。
    pub fn storage(mut self, storage: CoreStorageLocation) -> Self {
        self.storage = Some(storage);
        self
    }

    /// 构造 core 实例。
    ///
    /// 缺少任一必填字段返回 [`CoreError::MissingBuilderField`]。
    /// worker 线程由内部 `thread::spawn` 创建，构造期不会失败。
    pub fn build(self) -> Result<TiangongCore, CoreError> {
        let config = self
            .config
            .ok_or(CoreError::MissingBuilderField("config"))?;
        let session = self
            .session
            .ok_or(CoreError::MissingBuilderField("session"))?;
        let stream_tx = self
            .stream_tx
            .ok_or(CoreError::MissingBuilderField("event_sender"))?;
        let storage = self
            .storage
            .ok_or(CoreError::MissingBuilderField("storage"))?;
        Ok(TiangongCore::assemble(
            config,
            session,
            stream_tx,
            self.plugins,
            storage,
        ))
    }
}
