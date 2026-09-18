//! ensure / retire：Core 生命周期入口。
//!
//! `ensure_core` 复刻桌面 `app.rs` 的逻辑（issue #241/#234 已收窄）：先取创建锁，
//! 命中既有 Core 则 replace_config + 同步会话级运行配置；否则用 host 传入的 plugins
//! 构造新 TiangongCore 并插入 registry。`retire_core` 先 cancel（可选）再 take +
//! shutdown_join。

use std::sync::Arc;
use std::sync::mpsc::Sender;

use tiangong_core::agent_input::{AgentInput, AgentInputKind, MessageInput};
use tiangong_core::config::core::{CoreConfig, CoreConfigProvider};
use tiangong_core::core::{Plugin, TiangongCore};
use tiangong_llm::ModelEndpoint;
use tiangong_types::StreamEvent;

use crate::CoreManager;
use crate::core_manager::EnsuredCore;

/// 路由 Chat 槽位的默认模型端点。
fn default_chat_model(models: &tiangong_llm::models_config::ModelsConfig) -> ModelEndpoint {
    models
        .resolve_slot(tiangong_llm::models_config::RoutingSlot::Chat)
        .map(ModelEndpoint::from_resolved)
        .unwrap_or_default()
}

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
                core.set_reasoning_effort(session_config.reasoning_effort);
                return Ok(EnsuredCore {
                    session_id: session_id.to_string(),
                    is_new: false,
                });
            }
        }

        // 未命中：Core 构造即需持有实际模型——按 Session.model_ref 与当前
        // 模型注册表解析；失效或未设置时回退路由默认。解析不出任何可用
        // 模型时不静默兜底，由发送路径给出明确错误。
        let initial_model = self.resolve_initial_model(session_id);
        let plugins = build_plugins();
        let core = TiangongCore::builder()
            .session_id(session_id.to_string())
            .config(CoreConfigProvider::new(session_config.clone()))
            .trust_mode(session_config.trust_mode)
            .storage_root(self.storage_root.to_path_buf())
            .workspace_dir(workspace_dir)
            .stream_tx(stream_tx)
            .plugins(plugins)
            .model_endpoint(initial_model)
            .build();
        let id = core.session_id().to_string();
        self.registry().insert(id.clone(), core);
        Ok(EnsuredCore {
            session_id: id,
            is_new: true,
        })
    }

    /// Core 重建时的初始模型端点：按 Session.model_ref 与当前注册表解析。
    ///
    /// 用户选择失效（key 或 provider 已删）时给出**空端点**作为当前状态——
    /// 空端点与任何可用端点的 `model_key` 都不同，下一次用户选择有效模型
    /// 时 Manager 仍能识别出发生了切换；真正发送前的解析会给出明确报错，
    /// 不静默改成默认。
    fn resolve_initial_model(&self, session_id: &str) -> ModelEndpoint {
        let session_ref = self
            .load_session(session_id)
            .ok()
            .and_then(|session| session.model_ref);
        match session_ref {
            Some(key) => self.resolve_turn_model(Some(&key)).unwrap_or_default(),
            None => default_chat_model(&tiangong_config::io::load_models_config_at(
                &self.storage_root,
            )),
        }
    }

    /// 解析本轮的目标模型端点（`None` 表示跟随当前 Chat 默认）。
    ///
    /// `None` 只是选择策略，不能直接与 Core 当前模型比较——默认模型本身
    /// 会变化。必须先解析成实际目标再比较。
    pub fn resolve_turn_model(&self, model_ref: Option<&str>) -> Result<ModelEndpoint, String> {
        let models = tiangong_config::io::load_models_config_at(&self.storage_root);
        match model_ref.map(str::trim).filter(|key| !key.is_empty()) {
            Some(key) => {
                let entry = models.models.get(key).ok_or_else(|| {
                    format!("会话模型 {key} 已不在配置中，请重新选择模型或恢复该配置")
                })?;
                if !entry
                    .capabilities
                    .contains(&tiangong_llm::models_config::ModelCapability::Chat)
                {
                    return Err(format!("模型 {key} 不支持对话（缺少 Chat 能力）"));
                }
                let provider = models.providers.get(&entry.provider).ok_or_else(|| {
                    format!(
                        "会话模型 {key} 的服务提供方 {} 已删除，请重新选择模型或恢复该配置",
                        entry.provider
                    )
                })?;
                let api_key =
                    tiangong_llm::models_config::ModelsConfig::resolve_api_key(&provider.api_key);
                if api_key.trim().is_empty() {
                    return Err(format!(
                        "会话模型 {key} 的凭据未配置（服务提供方 {} 的 api_key 为空或其环境变量未设置）",
                        entry.provider
                    ));
                }
                Ok(ModelEndpoint::from_resolved(
                    tiangong_llm::models_config::ResolvedModel {
                        headers: provider.headers.clone(),
                        provider: entry.provider.clone(),
                        base_url: provider.base_url.clone(),
                        api_key,
                        timeout_ms: provider.timeout_ms,
                        protocol: provider.protocol,
                        model: entry.model.clone(),
                        options: entry.options.clone(),
                        context_window: entry.context_window,
                    },
                ))
            }
            None => {
                let target = default_chat_model(&models);
                if !target.is_usable() {
                    return Err("未配置可用的对话模型，请先在设置中配置模型".to_string());
                }
                Ok(target)
            }
        }
    }

    /// 模型切换编排：目标与 Core 当前实际模型不一致时，先用旧模型整理
    /// 上下文，再切换到新模型。任一步失败立即返回，不回滚、不继续。
    ///
    /// 调用方必须持有会话发送锁，避免并发请求交错压缩与切换。
    pub async fn switch_model_if_needed(
        &self,
        session_id: &str,
        target: ModelEndpoint,
    ) -> Result<(), String> {
        let core = {
            let registry = self.registry();
            registry.get(session_id).cloned()
        };
        let Some(core) = core else {
            return Err("会话无活跃 Core".to_string());
        };
        let current = core.current_endpoint();
        // 身份按端点派生（base_url + model + protocol）而非注册表 key：
        // 不同平台的同名 model id 必须区分，同一模型换了 key 或凭据则无需
        // 重新压缩切换。
        if current.is_same_model(&target) {
            return Ok(());
        }
        // 当前模型不可用（失效引用：Core 重建时端点为空）时
        // 跳过整理——用一个无法发请求的端点压缩必然失败，会把用户永久
        // 卡在失效状态。此时历史也从未被该模型处理过，直接切换即可。
        if current.is_usable() {
            // 用旧模型整理历史：新模型接手的是折叠后的上下文。
            core.compact_context().await.map_err(|error| match error {
                tiangong_core::core::CoreError::Busy => {
                    "会话正在执行，当前回合结束后可切换模型".to_string()
                }
                other => other.to_string(),
            })?;
        } else {
            tracing::info!(
                session_id,
                target_model = %target.model,
                "当前模型不可用，跳过切换前的上下文整理"
            );
        }
        core.switch_model(target).map_err(|error| match error {
            tiangong_core::core::CoreError::Busy => {
                "会话正在执行，当前回合结束后可切换模型".to_string()
            }
            other => other.to_string(),
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

    /// 取消指定会话的执行。
    ///
    /// 返回 true 表示 Cancel 已投递到活跃 turn task，false 表示当时没有可接受
    /// 取消命令的活跃 task；它不表示 turn 已经完成收尾。
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
    ///
    /// 投递成功且输入为用户消息时顺带触发标题自动生成（见 [`title`] 模块）；
    /// 标题生成在后台进行，不影响投递返回。
    pub fn deliver_to_core_if_live(&self, session_id: &str, input: AgentInputKind) -> bool {
        let user_text = match &input {
            AgentInputKind::Message(MessageInput::UserMessage { prepared, .. }) => prepared
                .iter()
                .find_map(|block| block.as_text().map(str::to_string)),
            _ => None,
        };
        let registry = self.registry();
        let delivered = registry
            .get(session_id)
            .is_some_and(|core| core.deliver(input).is_ok());
        if delivered && let Some(text) = user_text {
            self.spawn_title_generation_if_needed(session_id, &text);
        }
        delivered
    }

    /// 写入会话级对话模型引用（models 注册表 key；None 恢复跟随默认）。
    ///
    /// 轻量写入：只改引用不动执行端点——端点由投递前的端点校正按引用
    /// 切换，切走又切回时端点从未变化。运行中不写（core 忙即拒绝）。
    pub fn set_core_model_ref(
        &self,
        session_id: &str,
        model_ref: Option<String>,
    ) -> Result<(), String> {
        let registry = self.registry();
        let core = registry
            .get(session_id)
            .ok_or_else(|| "会话无活跃 Core".to_string())?;
        core.set_model_ref(model_ref).map_err(|error| match error {
            tiangong_core::core::CoreError::Busy => {
                "会话正在执行，当前回合结束后可切换模型".to_string()
            }
            other => format!("写入会话模型失败：{other}"),
        })
    }

    /// 设置指定会话 core 的信任模式（实时生效）。
    pub fn set_core_trust_mode(&self, session_id: &str, mode: tiangong_types::TrustMode) {
        let registry = self.registry();
        if let Some(core) = registry.get(session_id) {
            core.set_trust_mode(mode);
        }
    }

    /// 设置指定会话 Core 的思考强度（下一次尚未发出的模型请求生效）。
    pub fn set_core_reasoning_effort(
        &self,
        session_id: &str,
        effort: tiangong_llm::request::ReasoningEffort,
    ) {
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
