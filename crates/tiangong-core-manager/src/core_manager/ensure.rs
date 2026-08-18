//! ensure / retire：Core 生命周期入口。
//!
//! `ensure_core` 复刻桌面 `app.rs` 的逻辑（issue #241/#234 已收窄）：先取创建锁，
//! 命中既有 Core 则 replace_config + 同步会话级运行配置；否则用 host 传入的 plugins
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
    /// - 命中既有 Core：替换配置并同步会话级运行配置，返回 `is_new=false`
    /// - 未命中：调用 `build_plugins` 构造插件集合，构造全新 TiangongCore 并插入 registry
    ///
    /// `build_plugins` 是**按需回调**：只有 Core 不存在（需要新建）时才会被调用。
    /// 这样命中分支不会浪费一次完整的插件构造（含 WASM 实例化）。
    /// session 真相源是磁盘，Core 内部按需 `load_from_storage`。
    pub async fn ensure_core<F>(
        &self,
        session_id: &str,
        session_config: CoreConfig,
        workspace_dir: String,
        stream_tx: Sender<StreamEvent>,
        build_plugins: F,
    ) -> Result<EnsuredCore, String>
    where
        F: FnOnce() -> Vec<Arc<dyn Plugin>>,
    {
        let creation_lock = self.creation_lock(session_id);
        let _creation_guard = creation_lock.lock_owned().await;

        // 命中既有 Core：刷新配置和会话运行设置（cwd 由磁盘真相源维护，无需投递）。
        // build_plugins 回调不会被调用，避免每次发送都重新构造插件集合。
        {
            let registry = self.registry();
            if let Some(core) = registry.get(session_id) {
                let _ = core.replace_config(session_config.clone());
                core.set_trust_mode(session_config.trust_mode);
                core.set_reasoning_effort(session_config.reasoning_effort.clone());
                return Ok(EnsuredCore {
                    session_id: session_id.to_string(),
                    is_new: false,
                });
            }
        }

        // 未命中：按需构造插件集合，再构造全新 Core。
        let plugins = build_plugins();
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

    /// 响应交互请求（request_user 阻塞等待中）：注册表原子闭合后投递等待 turn。
    pub fn resolve_interaction_to_core(
        &self,
        session_id: &str,
        request_id: String,
        result_json: String,
    ) -> bool {
        self.deliver_to_core_if_live(
            session_id,
            AgentInputKind::resolve_interaction(request_id, result_json),
        )
    }

    /// 设置指定会话 core 的信任模式（实时生效）。
    pub fn set_core_trust_mode(&self, session_id: &str, mode: tiangong_types::TrustMode) {
        let registry = self.registry();
        if let Some(core) = registry.get(session_id) {
            core.set_trust_mode(mode);
        }
    }

    /// 设置指定会话 Core 的思考强度（下一次尚未发出的模型请求生效）。
    pub fn set_core_reasoning_effort(&self, session_id: &str, effort: String) {
        let registry = self.registry();
        if let Some(core) = registry.get(session_id) {
            core.set_reasoning_effort(effort);
        }
    }

    /// 获取指定会话 Core 全部插件贡献的 @提及候选。
    ///
    /// 经 [`TiangongCore::get_mentions`] 聚合（遍历 native + WASM 插件）。
    /// 会话不存在 Core 时返回空列表（mention 与会话绑定，无 Core 即无候选）。
    pub fn get_core_mentions(&self, session_id: &str) -> Vec<tiangong_types::MentionCandidate> {
        let registry = self.registry();
        registry
            .get(session_id)
            .map(|core| core.get_mentions())
            .unwrap_or_default()
    }

    /// 获取任意一个活跃 Core 的 @提及候选。
    ///
    /// mention 候选（skill/mcp 列表等）与具体会话内容无关——同一宿主内各 Core 注册
    /// 的插件集合一致，故任意活跃 Core 返回的结果相同。供无 session_id 上下文的
    /// 宿主命令（如 `get_mention_candidates`）使用；无任何活跃 Core 时返回空。
    pub fn get_any_mentions(&self) -> Vec<tiangong_types::MentionCandidate> {
        let registry = self.registry();
        registry
            .iter()
            .next()
            .map(|(_, core)| core.get_mentions())
            .unwrap_or_default()
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
