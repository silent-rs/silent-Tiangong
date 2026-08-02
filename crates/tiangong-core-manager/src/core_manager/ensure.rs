//! ensure / retire：Core 生命周期入口。
//!
//! `ensure_core` 复刻桌面 `app.rs` 的逻辑（issue #241/#234 已收窄）：先取创建锁，
//! 命中既有 Core 则 replace_config + 同步 trust_mode；否则用 host 传入的 plugins
//! 构造新 TiangongCore 并插入 registry。`retire_core` 先 cancel（可选）再 take +
//! shutdown_join。

use std::sync::Arc;
use std::sync::mpsc::Sender;

use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::core::{Plugin, TiangongCore};
use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
use tiangong_types::StreamEvent;

use crate::CoreManager;
use crate::core_manager::EnsuredCore;

impl CoreManager {
    /// 确保 registry 中存在该会话的 Core，返回是否新建。
    ///
    /// **线程安全保证**：覆盖同一会话从首次检查到 Core 插入的完整创建区间
    /// （`creation_lock`），避免落选 Core 仍执行插件恢复钩子。
    ///
    /// - 命中既有 Core：`replace_config(session_config)` + `set_trust_mode`，返回 `is_new=false`
    /// - 未命中：用 host 传入的 `plugins` 构造全新 TiangongCore 并插入 registry
    ///
    /// `plugins` 由 host 构造（桌面/服务端的插件集合差异大，不能在共享层硬编码）。
    /// session 真相源是磁盘，Core 内部按需 `load_from_storage`。
    pub async fn ensure_core(
        &self,
        session_id: &str,
        session_config: CoreConfig,
        workspace_dir: String,
        stream_tx: Sender<StreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Result<EnsuredCore, String> {
        let creation_lock = self.creation_lock(session_id);
        let _creation_guard = creation_lock.lock_owned().await;

        // 命中既有 Core：只刷新配置与 trust_mode（cwd 由磁盘真相源维护，无需投递）。
        {
            let registry = self.registry();
            if let Some(core) = registry.get(session_id) {
                let _ = core.replace_config(session_config.clone());
                core.set_trust_mode(session_config.trust_mode);
                return Ok(EnsuredCore {
                    session_id: session_id.to_string(),
                    is_new: false,
                });
            }
        }

        // 未命中：用 host 传入的 plugins 构造全新 Core。
        let core = TiangongCore::builder()
            .session_id(session_id.to_string())
            .config(CoreConfigProvider::new(session_config.clone()))
            .trust_mode(session_config.trust_mode)
            .storage_root(self.storage_root.to_path_buf())
            .workspace_dir(workspace_dir)
            .stream_tx(stream_tx)
            .plugins(plugins)
            .build();
        let id = core.session_id().to_string();
        self.registry().insert(id.clone(), core);
        Ok(EnsuredCore {
            session_id: id,
            is_new: true,
        })
    }

    /// 关闭并等待指定会话的 Core 结束。
    ///
    /// Core 的 worker join 是同步阻塞调用，本方法用 `spawn_blocking` 包裹以适配
    /// async 调用方。`cancel` 为 true 时先投递 `Command::Cancel` 再 take + join，
    /// 用于删除会话等需要主动终止在途 turn 的场景；失败回滚传 false（仅取走本次
    /// 绑定的 Core 并等其写盘结束）。Core 不存在时直接返回。
    pub async fn retire_core(&self, session_id: &str, cancel: bool) -> Result<(), String> {
        let creation_lock = self.creation_lock(session_id);
        let _creation_guard = creation_lock.lock_owned().await;
        self.retire_core_locked(session_id, cancel).await
    }

    pub(crate) async fn retire_core_locked(
        &self,
        session_id: &str,
        cancel: bool,
    ) -> Result<(), String> {
        if cancel {
            let _ = self.cancel_core(session_id);
        }
        let Some(core) = self.take_core(session_id) else {
            return Ok(());
        };
        let sid = session_id.to_string();
        match tokio::task::spawn_blocking(move || core.shutdown_join()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("关闭会话 {sid} 的 Core 失败：{error}")),
            Err(error) => Err(format!("等待会话 {sid} 的 Core 关闭失败：{error}")),
        }
    }

    /// 取消指定会话的执行（向活跃 turn task 投递 Cancel；无活跃 task 则忽略）。
    pub fn cancel_core(&self, session_id: &str) -> bool {
        let registry = self.registry();
        registry
            .get(session_id)
            .is_some_and(|core| core.deliver(AgentInputKind::cancel()).is_ok())
    }

    /// 取回会话 Core（消费，用于持久化或显式切换）。
    pub fn take_core(&self, session_id: &str) -> Option<TiangongCore> {
        self.registry().remove(session_id)
    }

    /// 仅当指定会话存在 Core 时投递输入。
    pub fn deliver_to_core_if_live(&self, session_id: &str, input: AgentInputKind) -> bool {
        let registry = self.registry();
        registry
            .get(session_id)
            .is_some_and(|core| core.deliver(input).is_ok())
    }

    /// 向指定会话的 core 发送审批响应。
    pub fn respond_approval_to_core(&self, session_id: &str, request_id: String, approved: bool) {
        self.deliver_to_core_if_live(session_id, AgentInputKind::approval(request_id, approved));
    }

    /// 设置指定会话 core 的信任模式（实时生效）。
    pub fn set_core_trust_mode(&self, session_id: &str, mode: tiangong_types::TrustMode) {
        let registry = self.registry();
        if let Some(core) = registry.get(session_id) {
            core.set_trust_mode(mode);
        }
    }

    /// 更新指定会话标题（落盘始终由 Core 负责，保证不与 turn 对 session 的读写竞争）。
    ///
    /// 必须存在 live Core（标题可编辑意味着 Core 应已创建）；不存在则视为异常并报错。
    /// Core 内部按 is_busy 分流：忙时投递 turn task，Core 空闲时 Core 自己写盘。
    ///
    /// `only_if_default=true` 时仅当当前标题仍是默认值才覆盖（lite 自动生成用，
    /// 用户手动改过则不覆盖）；用户手动编辑传 false。
    pub fn set_core_title(
        &self,
        session_id: &str,
        title: String,
        only_if_default: bool,
    ) -> Result<(), String> {
        let registry = self.registry();
        let Some(core) = registry.get(session_id) else {
            return Err(format!("会话 {session_id} 无可用 Core，无法更新标题"));
        };
        core.set_title(title, only_if_default)
            .map_err(|_| "更新会话标题失败".to_string())
    }
}
